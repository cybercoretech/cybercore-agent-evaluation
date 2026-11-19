//! Rootless Podman container runner (ADR-0015).
//!
//! Each attempt gets its own container from an allowlisted, locally present
//! image, run by image ID with `--pull never`: no network, all capabilities
//! dropped, `no-new-privileges`, a read-only root filesystem, size-limited
//! tmpfs mounts for `/workspace`, `/tmp` and (when collecting) `/workspace/
//! output`, and cgroup limits on memory, CPU and processes. The runner checks
//! that the limits really applied before running the task and refuses
//! otherwise. The task runs through `podman exec`; output files are read only
//! from a paused (frozen) container; the container is always stopped and
//! removed.

use crate::local_process::{
    collect_process_output, create_temporary_directory, invalid, is_reserved_environment_name,
    runner_error, unavailable, valid_environment_name, valid_logical_id,
    validate_requested_budgets, write_input_files, CollectedOutput, LocalProcessInputFile,
    SharedOutputBudget, MAX_CONFIGURED_ARGUMENT_BYTES, MAX_CONFIGURED_ARTIFACT_BYTES,
    MAX_CONFIGURED_INPUT_FILES, MAX_CONFIGURED_OUTPUT_FILES, MAX_CONFIGURED_WALL_TIME_MS,
    MAX_SINGLE_ARGUMENT_BYTES, OUTPUT_DRAIN_GRACE,
};
use crate::runner::{
    ArtifactManifest, ExecutionDisposition, ExecutionRequest, NetworkPolicy, RedactionState,
    ResourceUsage, Runner, RunnerCapabilities, RunnerDescriptor, RunnerError, RunnerErrorKind,
    RunnerEvent, RunnerFailure, RunnerFuture, RunnerResult, RunnerType, SideEffectPolicy,
};
use crate::workspace_outputs::{artifact_staging_dir, collect_outputs_into};
use chrono::{SecondsFormat, Utc};
use serde::Deserialize;
use serde_json::json;
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::os::unix::fs::DirBuilderExt;
use std::path::{Path, PathBuf};
use std::process::{Command as StdCommand, Stdio};
use std::sync::atomic::AtomicU64;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::process::Command;

const WORKSPACE: &str = "/workspace";
const OUTPUTS: &str = "/workspace/output";
const PODMAN_STEP_TIMEOUT: Duration = Duration::from_secs(60);
/// Extra lifetime for the container's own `sleep`, beyond the task budget, so
/// an orphaned container (runner crash) still stops and removes itself.
const SELF_EXPIRY_MARGIN_SECS: u64 = 120;

#[derive(Debug, Clone)]
pub struct ContainerRunnerConfig {
    pub runner_id: String,
    pub runner_version: String,
    /// Absolute path to `podman` (normally `/usr/bin/podman`).
    pub podman_path: PathBuf,
    /// Host directory for short-lived input files mounted read-only.
    pub temporary_root: PathBuf,
    /// Logical image IDs → local image references. Each is resolved to its
    /// image ID at construction; containers run by that ID. Images must
    /// already be present (the runner never pulls) and must provide `sleep`.
    pub images: BTreeMap<String, String>,
    /// Logical executable IDs → absolute paths inside the images.
    pub allowed_executables: BTreeMap<String, String>,
    pub allowed_environment_keys: BTreeSet<String>,
    pub max_wall_time_ms: u64,
    pub max_output_bytes: u64,
    /// cgroup memory limit (no swap) for each container.
    pub memory_bytes: u64,
    /// CPU limit in thousandths of a CPU (1000 = one CPU).
    pub cpu_millis: u32,
    /// cgroup process/thread limit for each container.
    pub pids_limit: u32,
    pub workspace_bytes: u64,
    pub tmp_bytes: u64,
    pub max_input_file_bytes: u64,
    pub max_input_files: usize,
    pub max_arguments: usize,
    pub max_argument_bytes: u64,
    /// Where collected `/workspace/output` files are staged; `None` disables
    /// collection.
    pub artifact_root: Option<PathBuf>,
    pub max_artifact_bytes: u64,
    pub max_output_files: usize,
}

/// `task_input` contract for the container runner.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContainerTaskInput {
    /// Logical image from the runner's `images`; optional when exactly one is configured.
    #[serde(default)]
    pub image_id: Option<String>,
    pub executable_id: String,
    #[serde(default)]
    pub args: Vec<String>,
    /// UTF-8 files exposed read-only under `/workspace/input`.
    #[serde(default)]
    pub files: Vec<LocalProcessInputFile>,
    #[serde(default)]
    pub environment: BTreeMap<String, String>,
    /// Mount a tmpfs at `/workspace/output` sized to the request's
    /// `max_artifact_bytes`, and collect the regular files left there.
    #[serde(default)]
    pub collect_outputs: bool,
}

#[derive(Debug, Clone)]
struct ResolvedImage {
    reference: String,
    image_id: String,
}

pub struct ContainerRunner {
    descriptor: RunnerDescriptor,
    capabilities: RunnerCapabilities,
    podman: PathBuf,
    temporary_root: PathBuf,
    images: BTreeMap<String, ResolvedImage>,
    allowed_executables: BTreeMap<String, String>,
    allowed_environment_keys: BTreeSet<String>,
    memory_bytes: u64,
    cpu_millis: u32,
    pids_limit: u32,
    workspace_bytes: u64,
    tmp_bytes: u64,
    max_input_file_bytes: u64,
    max_input_files: usize,
    max_arguments: usize,
    max_argument_bytes: u64,
    artifact_root: Option<PathBuf>,
    max_output_files: usize,
}

impl ContainerRunner {
    pub fn new(config: ContainerRunnerConfig) -> Result<Self, RunnerError> {
        if config.runner_id.trim().is_empty() {
            return Err(invalid("runner_id is required"));
        }
        if !config.podman_path.is_absolute() {
            return Err(invalid("podman_path must be absolute"));
        }
        if config.max_wall_time_ms == 0 || config.max_wall_time_ms > MAX_CONFIGURED_WALL_TIME_MS {
            return Err(invalid(
                "max_wall_time_ms must be between 1 ms and 60 minutes",
            ));
        }
        if config.max_output_bytes == 0 || config.max_output_bytes > 64 * 1024 * 1024 {
            return Err(invalid(
                "max_output_bytes must be between 1 byte and 64 MiB",
            ));
        }
        if config.memory_bytes < 16 * 1024 * 1024 {
            return Err(invalid("memory_bytes must be at least 16 MiB"));
        }
        if config.cpu_millis == 0 || config.pids_limit < 4 {
            return Err(invalid(
                "cpu_millis must be positive and pids_limit at least 4",
            ));
        }
        if config.workspace_bytes == 0 || config.tmp_bytes == 0 {
            return Err(invalid("workspace_bytes and tmp_bytes must be positive"));
        }
        if config.max_input_files > MAX_CONFIGURED_INPUT_FILES
            || config.max_argument_bytes > MAX_CONFIGURED_ARGUMENT_BYTES
        {
            return Err(invalid(
                "input or argument limits exceed the operator ceilings",
            ));
        }
        for (id, path) in &config.allowed_executables {
            if !valid_logical_id(id) || !path.starts_with('/') || path.contains('\0') {
                return Err(invalid(
                    "allowed_executables needs logical IDs mapped to absolute in-image paths",
                ));
            }
        }
        if config.allowed_executables.is_empty() || config.images.is_empty() {
            return Err(invalid(
                "at least one image and one executable must be allowlisted",
            ));
        }
        for key in &config.allowed_environment_keys {
            if !valid_environment_name(key) || is_reserved_environment_name(key) {
                return Err(invalid(
                    "environment allowlist contains an invalid or reserved name",
                ));
            }
        }
        fs::create_dir_all(&config.temporary_root)
            .map_err(|_| invalid("temporary_root could not be created"))?;
        let temporary_root = fs::canonicalize(&config.temporary_root)
            .map_err(|_| invalid("temporary_root could not be resolved"))?;
        let artifact_root = match &config.artifact_root {
            None => None,
            Some(root) => {
                fs::create_dir_all(root)
                    .map_err(|_| invalid("artifact_root could not be created"))?;
                if config.max_output_files == 0
                    || config.max_output_files > MAX_CONFIGURED_OUTPUT_FILES
                {
                    return Err(invalid("max_output_files must be between 1 and 10000"));
                }
                if config.max_artifact_bytes == 0
                    || config.max_artifact_bytes > MAX_CONFIGURED_ARTIFACT_BYTES
                {
                    return Err(invalid(
                        "max_artifact_bytes must be between 1 byte and 1 GiB",
                    ));
                }
                Some(
                    fs::canonicalize(root)
                        .map_err(|_| invalid("artifact_root could not be resolved"))?,
                )
            }
        };

        let version = StdCommand::new(&config.podman_path)
            .arg("--version")
            .stdin(Stdio::null())
            .output()
            .map_err(|_| unavailable("podman could not be executed"))?;
        if !version.status.success() {
            return Err(unavailable("podman --version failed"));
        }
        let mut images = BTreeMap::new();
        for (id, reference) in &config.images {
            if !valid_logical_id(id) {
                return Err(invalid(
                    "image IDs may contain only ASCII letters, digits, dot, underscore, and hyphen",
                ));
            }
            let inspect = StdCommand::new(&config.podman_path)
                .args(["image", "inspect", "--format", "{{.Id}}"])
                .arg(reference)
                .stdin(Stdio::null())
                .output()
                .map_err(|_| unavailable("podman could not be executed"))?;
            let image_id = String::from_utf8_lossy(&inspect.stdout).trim().to_string();
            if !inspect.status.success()
                || image_id.len() != 64
                || !image_id.bytes().all(|b| b.is_ascii_hexdigit())
            {
                return Err(invalid(format!(
                    "image {id} ({reference}) is not present locally; pull it first (the runner never pulls)"
                )));
            }
            images.insert(
                id.clone(),
                ResolvedImage {
                    reference: reference.clone(),
                    image_id,
                },
            );
        }

        let descriptor = RunnerDescriptor {
            runner_id: config.runner_id,
            runner_type: RunnerType::Local,
            runner_version: Some(config.runner_version),
        };
        let capabilities = RunnerCapabilities {
            supports_network_deny_all: true,
            supports_network_allowlist: false,
            supports_side_effect_deny_all: true,
            supports_side_effect_allowlist: false,
            max_wall_time_ms: Some(config.max_wall_time_ms),
            max_output_bytes: Some(config.max_output_bytes),
            max_artifact_bytes: artifact_root.as_ref().map(|_| config.max_artifact_bytes),
            max_tool_calls: None,
        };
        Ok(Self {
            descriptor,
            capabilities,
            podman: config.podman_path,
            temporary_root,
            images,
            allowed_executables: config.allowed_executables,
            allowed_environment_keys: config.allowed_environment_keys,
            memory_bytes: config.memory_bytes,
            cpu_millis: config.cpu_millis,
            pids_limit: config.pids_limit,
            workspace_bytes: config.workspace_bytes,
            tmp_bytes: config.tmp_bytes,
            max_input_file_bytes: config.max_input_file_bytes,
            max_input_files: config.max_input_files,
            max_arguments: config.max_arguments,
            max_argument_bytes: config.max_argument_bytes,
            artifact_root,
            max_output_files: config.max_output_files,
        })
    }

    fn parse_task_input(
        &self,
        request: &ExecutionRequest,
    ) -> Result<ContainerTaskInput, RunnerError> {
        let task: ContainerTaskInput = serde_json::from_value(request.task_input.clone())
            .map_err(|_| invalid("task_input must match the container invocation contract"))?;
        if !self.allowed_executables.contains_key(&task.executable_id) {
            return Err(invalid("task_input executable_id is not allowlisted"));
        }
        if task.args.len() > self.max_arguments {
            return Err(invalid(
                "task_input has more arguments than the configured limit",
            ));
        }
        let mut argument_bytes = 0_u64;
        for argument in &task.args {
            if argument.contains('\0') || argument.len() > MAX_SINGLE_ARGUMENT_BYTES {
                return Err(invalid(
                    "process arguments must be NUL-free and at most 65536 bytes",
                ));
            }
            argument_bytes += argument.len() as u64;
        }
        if argument_bytes > self.max_argument_bytes {
            return Err(invalid(
                "process arguments exceed the configured byte limit",
            ));
        }
        if task.files.len() > self.max_input_files {
            return Err(invalid(
                "task_input contains more files than the configured limit",
            ));
        }
        if task.environment.len() > 32 {
            return Err(invalid(
                "task_input contains more than 32 environment variables",
            ));
        }
        for (key, value) in &task.environment {
            if !self.allowed_environment_keys.contains(key) || value.contains('\0') {
                return Err(invalid(
                    "task_input requested an environment variable that is not allowlisted",
                ));
            }
        }
        if task.collect_outputs {
            if self.artifact_root.is_none() {
                return Err(invalid(
                    "task_input collect_outputs requires a runner configured with artifact_root",
                ));
            }
            if request.budget.max_artifact_bytes == 0 {
                return Err(invalid(
                    "task_input collect_outputs requires a positive max_artifact_bytes budget",
                ));
            }
        }
        Ok(task)
    }

    fn image(&self, task: &ContainerTaskInput) -> Result<&ResolvedImage, RunnerError> {
        match &task.image_id {
            Some(id) => self
                .images
                .get(id)
                .ok_or_else(|| invalid("task_input image_id is not allowlisted")),
            None if self.images.len() == 1 => self
                .images
                .values()
                .next()
                .ok_or_else(|| invalid("no image configured")),
            None => Err(invalid(
                "task_input image_id is required when several images are configured",
            )),
        }
    }

    async fn podman(&self, args: &[&str]) -> Result<String, RunnerError> {
        let output = tokio::time::timeout(
            PODMAN_STEP_TIMEOUT,
            Command::new(&self.podman)
                .args(args)
                .stdin(Stdio::null())
                .kill_on_drop(true)
                .output(),
        )
        .await
        .map_err(|_| unavailable("a podman step timed out"))?
        .map_err(|_| unavailable("podman could not be executed"))?;
        if !output.status.success() {
            return Err(runner_error(
                RunnerErrorKind::Unavailable,
                true,
                format!("podman {} failed", args.first().copied().unwrap_or("")),
            ));
        }
        Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
    }

    async fn execute_inner(&self, request: &ExecutionRequest) -> Result<RunnerResult, RunnerError> {
        validate_requested_budgets(
            request.budget.max_wall_time_ms,
            request.budget.max_output_bytes,
            self.capabilities.max_wall_time_ms.unwrap_or(0),
            self.capabilities.max_output_bytes.unwrap_or(0),
        )?;
        if request.network_policy != NetworkPolicy::DenyAll
            || request.side_effect_policy != SideEffectPolicy::DenyAll
        {
            return Err(runner_error(
                RunnerErrorKind::UnsupportedPolicy,
                false,
                "the container runner supports deny-all network and side-effect policies only",
            ));
        }
        let task = self.parse_task_input(request)?;
        let image = self.image(&task)?.clone();
        let executable = self
            .allowed_executables
            .get(&task.executable_id)
            .ok_or_else(|| invalid("task_input executable_id is not allowlisted"))?
            .clone();
        let staging = match (&self.artifact_root, task.collect_outputs) {
            (Some(root), true) => {
                let published = artifact_staging_dir(root, &request.request_id);
                if published.exists() {
                    return Err(invalid(
                        "outputs for this request_id were already collected",
                    ));
                }
                Some(published)
            }
            _ => None,
        };
        let input_directory = create_temporary_directory(&self.temporary_root)?;
        write_input_files(
            &input_directory.path,
            &task.files,
            self.max_input_file_bytes,
        )?;

        let name = container_name(&request.request_id);
        let mut guard = ContainerGuard::new(&self.podman, &name);
        let sleep_secs = request.budget.max_wall_time_ms / 1000 + SELF_EXPIRY_MARGIN_SECS;
        let memory = self.memory_bytes.to_string();
        let pids = self.pids_limit.to_string();
        let cpus = format!("{}.{:03}", self.cpu_millis / 1000, self.cpu_millis % 1000);
        let workspace = format!("{WORKSPACE}:rw,size={},mode=1777", self.workspace_bytes);
        let tmp = format!("/tmp:rw,size={},mode=1777", self.tmp_bytes);
        let outputs = format!(
            "{OUTPUTS}:rw,size={},mode=1777",
            request.budget.max_artifact_bytes
        );
        let input_mount = format!("{}:{WORKSPACE}/input:ro", input_directory.path.display());
        let label = format!("io.cybercore.request={name}");
        let sleep = sleep_secs.to_string();
        let mut run: Vec<&str> = vec![
            "run",
            "-d",
            "--rm",
            "--name",
            &name,
            "--pull",
            "never",
            "--network",
            "none",
            "--cap-drop",
            "all",
            "--security-opt",
            "no-new-privileges",
            "--read-only",
            "--userns",
            "keep-id",
            "--memory",
            &memory,
            "--memory-swap",
            &memory,
            "--pids-limit",
            &pids,
            "--cpus",
            &cpus,
            "--log-driver",
            "none",
            "--no-hosts",
            "--tmpfs",
            &workspace,
            "--tmpfs",
            &tmp,
        ];
        if staging.is_some() {
            run.extend(["--tmpfs", &outputs]);
        }
        run.extend([
            "-v",
            &input_mount,
            "--workdir",
            WORKSPACE,
            "--label",
            &label,
            &image.image_id,
            "sleep",
            &sleep,
        ]);
        self.podman(&run).await?;
        guard.armed = true;

        let pid = self
            .podman(&["inspect", "--format", "{{.State.Pid}}", &name])
            .await?;
        let cgroup = self
            .podman(&["inspect", "--format", "{{.State.CgroupPath}}", &name])
            .await?;
        let cgroup_dir = PathBuf::from("/sys/fs/cgroup").join(cgroup.trim_start_matches('/'));
        verify_limits(&cgroup_dir, self.memory_bytes, self.pids_limit)?;

        let mut exec = Command::new(&self.podman);
        exec.args(["exec", "--workdir", WORKSPACE]);
        for (key, value) in &task.environment {
            exec.arg("--env").arg(format!("{key}={value}"));
        }
        exec.arg(&name)
            .arg(&executable)
            .args(&task.args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);

        let started_at = Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true);
        let started = Instant::now();
        let mut child = exec
            .spawn()
            .map_err(|_| unavailable("could not start podman exec"))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| unavailable("stdout pipe was unavailable"))?;
        let stderr = child
            .stderr
            .take()
            .ok_or_else(|| unavailable("stderr pipe was unavailable"))?;
        let budget = Arc::new(SharedOutputBudget {
            used: AtomicU64::new(0),
            maximum: request.budget.max_output_bytes,
        });
        let stdout_task =
            tokio::spawn(crate::local_process::capture_output(stdout, budget.clone()));
        let stderr_task = tokio::spawn(crate::local_process::capture_output(stderr, budget));
        let CollectedOutput {
            exit_status,
            stdout: stdout_capture,
            stderr: stderr_capture,
            timed_out,
            output_limit_exceeded,
            drain_timed_out,
        } = collect_process_output(
            &mut child,
            stdout_task,
            stderr_task,
            Duration::from_millis(request.budget.max_wall_time_ms),
            OUTPUT_DRAIN_GRACE,
        )
        .await?;
        let elapsed_ms = started.elapsed().as_millis().min(u64::MAX as u128) as u64;
        let timed_out = timed_out || elapsed_ms > request.budget.max_wall_time_ms;
        let exit_code = exit_status.and_then(|s| s.code());
        let oom_killed = oom_kills(&cgroup_dir) > 0;

        let (mut disposition, mut failure, mut event_type) = if timed_out {
            (ExecutionDisposition::TimedOut, None, "execution_timed_out")
        } else if output_limit_exceeded {
            errored(
                "output_limit_exceeded",
                "process output exceeded the configured capture limit; the container was stopped",
            )
        } else if drain_timed_out {
            errored("output_drain_timed_out", "process output did not close within the drain grace period; unread output was discarded")
        } else if oom_killed {
            errored(
                "memory_limit_exceeded",
                "a process in the container was killed for exceeding the memory limit",
            )
        } else if exit_code == Some(0) {
            (ExecutionDisposition::Finished, None, "execution_finished")
        } else {
            errored(
                "process_exit_nonzero",
                "the allowlisted process exited unsuccessfully",
            )
        };

        // Outputs are read only from a frozen container, so no process can
        // change them while the host reads; killed runs are never collected.
        let mut artifacts = Vec::new();
        let mut artifact_bytes = 0;
        let mut output_files = Vec::new();
        let mut rejected = Vec::new();
        let terminated = timed_out || output_limit_exceeded || drain_timed_out;
        let export = match &staging {
            None => "not_requested".to_string(),
            Some(_) if terminated => "skipped_after_termination".to_string(),
            Some(published) => {
                self.podman(&["pause", &name]).await?;
                let source = PathBuf::from(format!("/proc/{pid}/root{OUTPUTS}"));
                let partial = PathBuf::from(format!("{}.partial", published.display()));
                let _ = fs::remove_dir_all(&partial);
                fs::DirBuilder::new()
                    .mode(0o700)
                    .create(&partial)
                    .map_err(|_| unavailable("could not create the output staging directory"))?;
                match collect_outputs_into(
                    &source,
                    &partial,
                    request.budget.max_artifact_bytes,
                    self.max_output_files,
                ) {
                    Ok(collected) if collected.limit_exceeded.is_some() => {
                        let _ = fs::remove_dir_all(&partial);
                        (disposition, failure, event_type) = errored(
                            "artifact_limit_exceeded",
                            &format!(
                                "workspace outputs were discarded: {}",
                                collected.limit_exceeded.unwrap_or_default()
                            ),
                        );
                        "discarded_over_limit".to_string()
                    }
                    Ok(collected) => {
                        fs::rename(&partial, published).map_err(|_| {
                            unavailable("could not publish collected workspace outputs")
                        })?;
                        for file in &collected.files {
                            artifacts.push(ArtifactManifest {
                                artifact_id: file.path.clone(),
                                content_type: "application/octet-stream".into(),
                                byte_length: file.byte_length,
                                sha256_hex: file.sha256_hex.clone(),
                                redaction_state: RedactionState::NotRequired,
                            });
                            output_files.push(json!({"path": file.path, "byte_length": file.byte_length, "sha256": file.sha256_hex}));
                        }
                        for (path, reason) in &collected.rejected {
                            rejected.push(json!({"path": path, "reason": reason}));
                        }
                        artifact_bytes = collected.total_bytes;
                        "collected".to_string()
                    }
                    Err(_) => {
                        let _ = fs::remove_dir_all(&partial);
                        (disposition, failure, event_type) = errored(
                            "artifact_collection_failed",
                            "workspace outputs could not be read; they were discarded",
                        );
                        "failed".to_string()
                    }
                }
            }
        };

        guard.stop().await;
        drop(input_directory);

        let ended_at = Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true);
        let stdout_text = String::from_utf8_lossy(&stdout_capture.bytes).into_owned();
        let stderr_text = String::from_utf8_lossy(&stderr_capture.bytes).into_owned();
        let output_truncated = stdout_capture.truncated || stderr_capture.truncated;
        let output_bytes = (stdout_capture.bytes.len() + stderr_capture.bytes.len()) as u64;
        let output = json!({
            "executable_id": task.executable_id,
            "exit_code": exit_code,
            "stdout": stdout_text,
            "stderr": stderr_text,
            "output_encoding": "utf-8-lossy",
            "output_truncated": output_truncated,
            "output_drain_timed_out": drain_timed_out,
            "stdout_stderr_redaction": "not_implemented",
            "unmeasured_usage_fields": ["input_tokens", "output_tokens", "tool_calls"],
            "workspace_persisted": false,
            "workspace_file_artifact_export": export,
            "workspace_output_files": output_files,
            "rejected_output_entries": rejected,
            "container": {
                "image": task.image_id.clone().unwrap_or_default(),
                "image_reference": image.reference,
                "image_digest": format!("sha256:{}", image.image_id),
                "memory_limit_bytes": self.memory_bytes,
                "cpu_millis": self.cpu_millis,
                "pids_limit": self.pids_limit,
                "oom_killed": oom_killed,
            },
        });
        Ok(RunnerResult {
            request_id: request.request_id.clone(),
            disposition,
            events: vec![
                RunnerEvent {
                    sequence: 1,
                    occurred_at: started_at,
                    actor: "container_runner".into(),
                    event_type: "execution_started".into(),
                    payload: json!({"executable_id": task.executable_id, "image_digest": format!("sha256:{}", image.image_id)}),
                },
                RunnerEvent {
                    sequence: 2,
                    occurred_at: ended_at,
                    actor: "container_runner".into(),
                    event_type: event_type.into(),
                    payload: json!({"exit_code": exit_code, "timed_out": timed_out, "oom_killed": oom_killed}),
                },
            ],
            artifacts,
            resource_usage: ResourceUsage {
                wall_time_ms: elapsed_ms,
                output_bytes,
                artifact_bytes,
                input_tokens: 0,
                output_tokens: 0,
                tool_calls: 0,
            },
            output: Some(output),
            failure,
        })
    }
}

impl Runner for ContainerRunner {
    fn descriptor(&self) -> &RunnerDescriptor {
        &self.descriptor
    }

    fn capabilities(&self) -> &RunnerCapabilities {
        &self.capabilities
    }

    fn execute<'a>(&'a self, request: &'a ExecutionRequest) -> RunnerFuture<'a> {
        Box::pin(async move { self.execute_inner(request).await })
    }
}

fn errored(
    code: &str,
    message: &str,
) -> (ExecutionDisposition, Option<RunnerFailure>, &'static str) {
    (
        ExecutionDisposition::Errored,
        Some(RunnerFailure {
            code: code.into(),
            message: message.into(),
            retryable: false,
        }),
        "execution_errored",
    )
}

/// The container name used for a request: derived from the request ID's
/// SHA-256, so arbitrary IDs stay valid and collision-free. Operators can use
/// it to find a run's container (also labelled `io.cybercore.request=<name>`).
pub fn container_name(request_id: &str) -> String {
    let digest = Sha256::digest(request_id.as_bytes());
    let hex: String = digest.iter().take(10).map(|b| format!("{b:02x}")).collect();
    format!("cybercore-run-{hex}")
}

/// Refuses to run unless the container's cgroup really enforces the limits
/// (hosts without cgroup v2 delegation would otherwise run unconstrained).
fn verify_limits(cgroup: &Path, memory: u64, pids: u32) -> Result<(), RunnerError> {
    let read = |file: &str| {
        fs::read_to_string(cgroup.join(file))
            .ok()
            .map(|s| s.trim().to_string())
    };
    let memory_ok = read("memory.max").is_some_and(|v| v == memory.to_string());
    let pids_ok = read("pids.max").is_some_and(|v| v == pids.to_string());
    if memory_ok && pids_ok {
        Ok(())
    } else {
        Err(unavailable(
            "container memory/pids limits were not applied (is cgroup v2 delegated to this user?); refusing to run without them",
        ))
    }
}

fn oom_kills(cgroup: &Path) -> u64 {
    fs::read_to_string(cgroup.join("memory.events"))
        .ok()
        .and_then(|events| {
            events.lines().find_map(|line| {
                line.strip_prefix("oom_kill ")
                    .and_then(|n| n.trim().parse().ok())
            })
        })
        .unwrap_or(0)
}

/// Stops and removes the container synchronously on every exit path,
/// including errors and cancellation (`--rm` alone removes asynchronously).
struct ContainerGuard {
    podman: PathBuf,
    name: String,
    armed: bool,
}

impl ContainerGuard {
    fn new(podman: &Path, name: &str) -> Self {
        Self {
            podman: podman.to_path_buf(),
            name: name.to_string(),
            armed: false,
        }
    }

    async fn stop(&mut self) {
        if self.armed {
            let _ = Command::new(&self.podman)
                .args(["rm", "--force", "--time", "0", "--ignore", &self.name])
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .await;
            self.armed = false;
        }
    }
}

impl Drop for ContainerGuard {
    fn drop(&mut self) {
        if self.armed {
            let _ = StdCommand::new(&self.podman)
                .args(["rm", "--force", "--time", "0", "--ignore", &self.name])
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn container_names_are_stable_and_safe() {
        let a = container_name("REQ ../../x");
        assert_eq!(a, container_name("REQ ../../x"));
        assert!(a.starts_with("cybercore-run-") && a.len() == "cybercore-run-".len() + 20);
        assert!(a.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-'));
        assert_ne!(a, container_name("REQ-2"));
    }

    #[test]
    fn missing_or_wrong_cgroup_limits_are_refused() {
        let dir = std::env::temp_dir().join(format!("cybercore-cg-test-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        assert!(verify_limits(&dir, 1024, 8).is_err());
        fs::write(dir.join("memory.max"), "max\n").unwrap();
        fs::write(dir.join("pids.max"), "8\n").unwrap();
        assert!(
            verify_limits(&dir, 1024, 8).is_err(),
            "unlimited memory must be refused"
        );
        fs::write(dir.join("memory.max"), "1024\n").unwrap();
        assert!(verify_limits(&dir, 1024, 8).is_ok());
        fs::write(dir.join("memory.events"), "low 0\noom 1\noom_kill 2\n").unwrap();
        assert_eq!(oom_kills(&dir), 2);
        fs::remove_dir_all(dir).unwrap();
    }
}
