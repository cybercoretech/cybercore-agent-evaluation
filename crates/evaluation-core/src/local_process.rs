//! Linux local process runner backed by Bubblewrap.
//!
//! This runner is intentionally narrow: it launches one pre-allowlisted executable
//! without a shell, gives it a bounded tmpfs workspace, mounts supplied inputs
//! read-only, isolates network/PID/IPC/UTS namespaces, and captures bounded output.
//! It does not export workspace files as artifacts; that requires the evidence-store
//! adapter and an explicit artifact redaction policy.

use crate::runner::{
    ArtifactManifest, ExecutionDisposition, ExecutionRequest, NetworkPolicy, RedactionState,
    ResourceUsage, Runner, RunnerCapabilities, RunnerDescriptor, RunnerError, RunnerErrorKind,
    RunnerEvent, RunnerFailure, RunnerFuture, RunnerResult, RunnerType, SideEffectPolicy,
};
use crate::workspace_outputs::{artifact_staging_dir, collect_outputs};
use chrono::{SecondsFormat, Utc};
use serde::Deserialize;
use serde_json::json;
use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::os::unix::fs::DirBuilderExt;
use std::path::{Component, Path, PathBuf};
use std::process::{Command as StdCommand, ExitStatus, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tokio::io::AsyncReadExt;
use tokio::process::Command;

static TEMP_SEQUENCE: AtomicU64 = AtomicU64::new(1);

const SANDBOX_WORKSPACE: &str = "/workspace";
const SANDBOX_INPUTS: &str = "/workspace/input";
const SANDBOX_HOME: &str = "/home/runner";
const SANDBOX_PATH: &str = "/usr/bin:/bin";
pub(crate) const MAX_CONFIGURED_WALL_TIME_MS: u64 = 3_600_000;
pub(crate) const MAX_CONFIGURED_INPUT_FILES: usize = 1024;
pub(crate) const MAX_CONFIGURED_ARGUMENT_BYTES: u64 = 512 * 1024;
pub(crate) const MAX_CONFIGURED_OUTPUT_FILES: usize = 10_000;
pub(crate) const MAX_CONFIGURED_ARTIFACT_BYTES: u64 = 1024 * 1024 * 1024;
const SANDBOX_OUTPUTS: &str = "/workspace/output";
pub(crate) const MAX_SINGLE_ARGUMENT_BYTES: usize = 64 * 1024;
const MAX_INPUT_PATH_BYTES: usize = 4096;
const MAX_INPUT_PATH_COMPONENT_BYTES: usize = 255;
/// How long stdout/stderr may stay open after the sandbox process exits.
pub(crate) const OUTPUT_DRAIN_GRACE: Duration = Duration::from_secs(2);

#[derive(Debug, Clone)]
pub struct LocalProcessRunnerConfig {
    pub runner_id: String,
    pub runner_version: String,
    /// Absolute path to the Bubblewrap executable (normally `/usr/bin/bwrap`).
    pub bubblewrap_path: PathBuf,
    /// Host directory used only for short-lived, size-limited input files.
    pub temporary_root: PathBuf,
    /// Logical executable IDs mapped to trusted host executable paths.
    pub allowed_executables: BTreeMap<String, PathBuf>,
    /// Additional task environment names the operator has explicitly approved.
    pub allowed_environment_keys: BTreeSet<String>,
    pub max_wall_time_ms: u64,
    pub max_output_bytes: u64,
    /// Maximum writable per-run workspace tmpfs size.
    pub max_workspace_bytes: u64,
    /// Maximum per-run `/tmp` tmpfs size.
    pub max_tmp_bytes: u64,
    /// Maximum aggregate UTF-8 input-file bytes accepted for one run.
    pub max_input_file_bytes: u64,
    pub max_input_files: usize,
    pub max_arguments: usize,
    pub max_argument_bytes: u64,
    /// Host directory where files a task writes to `/workspace/output` are
    /// staged after the run (only for tasks with `collect_outputs: true`).
    /// `None` disables output collection.
    pub artifact_root: Option<PathBuf>,
    /// Ceiling for a request's `max_artifact_bytes` when collection is enabled.
    pub max_artifact_bytes: u64,
    /// Most files one run may leave in `/workspace/output`.
    pub max_output_files: usize,
}

#[derive(Debug)]
pub struct LocalProcessRunner {
    descriptor: RunnerDescriptor,
    capabilities: RunnerCapabilities,
    bubblewrap_path: PathBuf,
    temporary_root: PathBuf,
    allowed_executables: BTreeMap<String, PathBuf>,
    allowed_environment_keys: BTreeSet<String>,
    max_input_file_bytes: u64,
    max_input_files: usize,
    max_arguments: usize,
    max_argument_bytes: u64,
    max_workspace_bytes: u64,
    max_tmp_bytes: u64,
    artifact_root: Option<PathBuf>,
    max_output_files: usize,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LocalProcessTaskInput {
    /// Logical key from `LocalProcessRunnerConfig.allowed_executables`.
    pub executable_id: String,
    #[serde(default)]
    pub args: Vec<String>,
    /// UTF-8 files exposed read-only under `/workspace/input`.
    #[serde(default)]
    pub files: Vec<LocalProcessInputFile>,
    /// Values are accepted only for keys in the runner's configured allowlist.
    #[serde(default)]
    pub environment: BTreeMap<String, String>,
    /// Mount a per-run host directory read-write at `/workspace/output` and
    /// collect the regular files left there as artifacts. Requires the runner's
    /// `artifact_root` and a positive `max_artifact_bytes` budget.
    #[serde(default)]
    pub collect_outputs: bool,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LocalProcessInputFile {
    /// Relative path below `/workspace/input`; traversal and absolute paths are rejected.
    pub path: String,
    pub content: String,
}

#[derive(Debug)]
pub(crate) struct TemporaryInputDirectory {
    pub(crate) path: PathBuf,
}

impl Drop for TemporaryInputDirectory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

/// A run's `/workspace/output` host directory: written as `<hash>.partial`
/// during the run, renamed to `<hash>` once its files are accepted.
struct OutputStaging {
    partial: PathBuf,
    published: PathBuf,
}

impl OutputStaging {
    fn publish(&self) -> Result<(), RunnerError> {
        fs::rename(&self.partial, &self.published)
            .map_err(|_| unavailable("could not publish collected workspace outputs"))
    }

    fn discard(&self) {
        let _ = fs::remove_dir_all(&self.partial);
    }
}

#[derive(Debug)]
pub(crate) struct OutputCapture {
    pub(crate) bytes: Vec<u8>,
    pub(crate) truncated: bool,
}

#[derive(Debug)]
pub(crate) struct SharedOutputBudget {
    pub(crate) used: AtomicU64,
    pub(crate) maximum: u64,
}

impl SharedOutputBudget {
    fn reserve(&self, requested: usize) -> usize {
        let requested = requested as u64;
        let mut current = self.used.load(Ordering::Relaxed);
        loop {
            let available = self.maximum.saturating_sub(current);
            let accepted = requested.min(available);
            match self.used.compare_exchange_weak(
                current,
                current + accepted,
                Ordering::Relaxed,
                Ordering::Relaxed,
            ) {
                Ok(_) => return accepted as usize,
                Err(actual) => current = actual,
            }
        }
    }
}

impl LocalProcessRunner {
    /// Construct the runner and perform a real Bubblewrap namespace preflight.
    /// Construction fails rather than falling back to an unsandboxed process.
    pub fn new(config: LocalProcessRunnerConfig) -> Result<Self, RunnerError> {
        if !cfg!(target_os = "linux") {
            return Err(runner_error(
                RunnerErrorKind::UnsupportedPolicy,
                false,
                "the Bubblewrap local process runner is supported only on Linux",
            ));
        }
        if config.runner_id.trim().is_empty() || config.runner_version.trim().is_empty() {
            return Err(invalid("runner_id and runner_version must be non-empty"));
        }
        if config.max_wall_time_ms == 0
            || config.max_output_bytes == 0
            || config.max_workspace_bytes == 0
            || config.max_tmp_bytes == 0
            || config.max_input_file_bytes == 0
            || config.max_input_files == 0
            || config.max_arguments == 0
            || config.max_argument_bytes == 0
        {
            return Err(invalid("all local runner limits must be greater than zero"));
        }
        if config.max_wall_time_ms > MAX_CONFIGURED_WALL_TIME_MS
            || config.max_output_bytes > 64 * 1024 * 1024
            || config.max_workspace_bytes > 256 * 1024 * 1024
            || config.max_tmp_bytes > 64 * 1024 * 1024
            || config.max_input_file_bytes > 256 * 1024 * 1024
            || config.max_input_files > MAX_CONFIGURED_INPUT_FILES
            || config.max_arguments > 4096
            || config.max_argument_bytes > MAX_CONFIGURED_ARGUMENT_BYTES
        {
            return Err(invalid(
                "a local runner limit exceeds its hard safety ceiling",
            ));
        }
        if config.allowed_executables.is_empty() {
            return Err(invalid("at least one allowlisted executable is required"));
        }

        let bubblewrap_path = fs::canonicalize(&config.bubblewrap_path)
            .map_err(|_| unavailable("Bubblewrap executable could not be resolved"))?;
        if !bubblewrap_path.is_file() || !is_executable(&bubblewrap_path) {
            return Err(unavailable("Bubblewrap path is not an executable file"));
        }

        fs::create_dir_all(&config.temporary_root).map_err(|_| {
            runner_error(
                RunnerErrorKind::Internal,
                false,
                "could not create local runner temporary root",
            )
        })?;
        let temporary_root = fs::canonicalize(&config.temporary_root).map_err(|_| {
            runner_error(
                RunnerErrorKind::Internal,
                false,
                "could not resolve local runner temporary root",
            )
        })?;
        if !temporary_root.is_dir() {
            return Err(invalid("temporary_root must be a directory"));
        }

        let mut allowed_executables = BTreeMap::new();
        for (executable_id, executable_path) in config.allowed_executables {
            if !valid_logical_id(&executable_id) {
                return Err(invalid("executable IDs may contain only ASCII letters, digits, dot, underscore, and hyphen"));
            }
            let canonical = fs::canonicalize(&executable_path)
                .map_err(|_| invalid("an allowlisted executable path could not be resolved"))?;
            if !canonical.is_file() || !is_executable(&canonical) {
                return Err(invalid(
                    "every allowlisted executable must resolve to an executable file",
                ));
            }
            // The sandbox exposes the host's read-only /usr tree. Do not silently
            // widen the visible filesystem for binaries from /opt, /home, /nix, etc.
            if !canonical.starts_with("/usr") {
                return Err(invalid("allowlisted executables must resolve beneath /usr"));
            }
            allowed_executables.insert(executable_id, canonical);
        }

        for key in &config.allowed_environment_keys {
            if !valid_environment_name(key) || is_reserved_environment_name(key) {
                return Err(invalid(
                    "environment allowlist contains an invalid or reserved variable name",
                ));
            }
        }

        let artifact_root = match &config.artifact_root {
            None => None,
            Some(root) => {
                fs::create_dir_all(root)
                    .map_err(|_| invalid("artifact_root could not be created"))?;
                let root = fs::canonicalize(root)
                    .map_err(|_| invalid("artifact_root could not be resolved"))?;
                if !root.is_dir() {
                    return Err(invalid("artifact_root must be a directory"));
                }
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
                Some(root)
            }
        };

        preflight_bubblewrap(&bubblewrap_path)?;

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
            // Without an artifact root, workspace files are ephemeral and any
            // collect_outputs request is rejected before launch.
            max_artifact_bytes: artifact_root.as_ref().map(|_| config.max_artifact_bytes),
            max_tool_calls: None,
        };

        Ok(Self {
            descriptor,
            capabilities,
            bubblewrap_path,
            temporary_root,
            allowed_executables,
            allowed_environment_keys: config.allowed_environment_keys,
            max_input_file_bytes: config.max_input_file_bytes,
            max_input_files: config.max_input_files,
            max_arguments: config.max_arguments,
            max_argument_bytes: config.max_argument_bytes,
            max_workspace_bytes: config.max_workspace_bytes,
            max_tmp_bytes: config.max_tmp_bytes,
            artifact_root,
            max_output_files: config.max_output_files,
        })
    }

    fn parse_task_input(
        &self,
        request: &ExecutionRequest,
    ) -> Result<LocalProcessTaskInput, RunnerError> {
        let task: LocalProcessTaskInput = serde_json::from_value(request.task_input.clone())
            .map_err(|_| invalid("task_input must match the local-process invocation contract"))?;
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
            if argument.contains('\0') {
                return Err(invalid("process arguments must not contain NUL bytes"));
            }
            if argument.len() > MAX_SINGLE_ARGUMENT_BYTES {
                return Err(invalid(
                    "a process argument exceeds the 65536-byte per-argument limit",
                ));
            }
            argument_bytes = argument_bytes
                .checked_add(argument.len() as u64)
                .ok_or_else(|| invalid("argument byte count overflow"))?;
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
        let environment_bytes = task
            .environment
            .iter()
            .try_fold(0_u64, |total, (key, value)| {
                total
                    .checked_add(key.len() as u64)?
                    .checked_add(value.len() as u64)
            })
            .ok_or_else(|| invalid("environment byte count overflow"))?;
        if environment_bytes > 16_384 {
            return Err(invalid("task environment exceeds the 16384-byte limit"));
        }
        for (key, value) in &task.environment {
            if !self.allowed_environment_keys.contains(key) {
                return Err(invalid(
                    "task_input requested an environment variable that is not allowlisted",
                ));
            }
            if value.contains('\0') {
                return Err(invalid(
                    "environment variable values must not contain NUL bytes",
                ));
            }
        }
        Ok(task)
    }

    fn prepare_inputs(
        &self,
        files: &[LocalProcessInputFile],
    ) -> Result<TemporaryInputDirectory, RunnerError> {
        let directory = create_temporary_directory(&self.temporary_root)?;
        write_input_files(&directory.path, files, self.max_input_file_bytes)?;
        Ok(directory)
    }

    fn prepare_output_staging(
        &self,
        request: &ExecutionRequest,
    ) -> Result<OutputStaging, RunnerError> {
        let root = self.artifact_root.as_ref().ok_or_else(|| {
            invalid("task_input collect_outputs requires a runner configured with artifact_root")
        })?;
        if request.budget.max_artifact_bytes == 0 {
            return Err(invalid(
                "task_input collect_outputs requires a positive max_artifact_bytes budget",
            ));
        }
        let published = artifact_staging_dir(root, &request.request_id);
        if published.exists() {
            return Err(invalid(
                "outputs for this request_id were already collected",
            ));
        }
        let mut partial = published.clone().into_os_string();
        partial.push(".partial");
        let partial = PathBuf::from(partial);
        // A stale partial directory can only come from an earlier crash of
        // this same request; it is ours to replace.
        let _ = fs::remove_dir_all(&partial);
        fs::DirBuilder::new()
            .mode(0o700)
            .create(&partial)
            .map_err(|_| unavailable("could not create the output staging directory"))?;
        Ok(OutputStaging { partial, published })
    }

    fn build_command(
        &self,
        executable: &Path,
        task: &LocalProcessTaskInput,
        input_dir: &Path,
        output_dir: Option<&Path>,
        workspace_bytes: u64,
        tmp_bytes: u64,
    ) -> Command {
        let mut command = Command::new(&self.bubblewrap_path);
        command
            .env_clear()
            .current_dir(&self.temporary_root)
            .kill_on_drop(true);
        append_bwrap_prefix(&mut command);
        command
            .arg("--size")
            .arg(tmp_bytes.to_string())
            .arg("--tmpfs")
            .arg("/tmp")
            .arg("--size")
            .arg(workspace_bytes.to_string())
            .arg("--tmpfs")
            .arg(SANDBOX_WORKSPACE)
            .arg("--ro-bind")
            .arg(input_dir)
            .arg(SANDBOX_INPUTS);
        if let Some(output_dir) = output_dir {
            command.arg("--bind").arg(output_dir).arg(SANDBOX_OUTPUTS);
        }
        command
            .arg("--dir")
            .arg("/home")
            .arg("--dir")
            .arg(SANDBOX_HOME)
            .arg("--clearenv")
            .arg("--setenv")
            .arg("PATH")
            .arg(SANDBOX_PATH)
            .arg("--setenv")
            .arg("HOME")
            .arg(SANDBOX_HOME)
            .arg("--setenv")
            .arg("PWD")
            .arg(SANDBOX_WORKSPACE)
            .arg("--setenv")
            .arg("TMPDIR")
            .arg("/tmp")
            .arg("--setenv")
            .arg("LANG")
            .arg("C.UTF-8")
            .arg("--setenv")
            .arg("LC_ALL")
            .arg("C")
            .arg("--setenv")
            .arg("TZ")
            .arg("UTC")
            .arg("--chdir")
            .arg(SANDBOX_WORKSPACE);
        for (key, value) in &task.environment {
            command.arg("--setenv").arg(key).arg(value);
        }
        command.arg("--").arg(executable).args(&task.args);
        command.stdin(Stdio::null());
        command.stdout(Stdio::piped());
        command.stderr(Stdio::piped());
        command
    }

    async fn execute_inner(&self, request: &ExecutionRequest) -> Result<RunnerResult, RunnerError> {
        // Enforce this adapter's advertised maxima even when a caller invokes
        // Runner::execute directly instead of using execute_with_runner.
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
                "the local Bubblewrap runner supports deny-all network and side-effect policies only",
            ));
        }
        let task = self.parse_task_input(request)?;
        let executable = self
            .allowed_executables
            .get(&task.executable_id)
            .ok_or_else(|| invalid("task_input executable_id is not allowlisted"))?;
        let input_directory = self.prepare_inputs(&task.files)?;
        let output_staging = if task.collect_outputs {
            Some(self.prepare_output_staging(request)?)
        } else {
            None
        };
        let mut command = self.build_command(
            executable,
            &task,
            &input_directory.path,
            output_staging.as_ref().map(|s| s.partial.as_path()),
            self.max_workspace_bytes,
            self.max_tmp_bytes,
        );

        let started_at = Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true);
        let started = Instant::now();
        let mut child = command.spawn().map_err(|_| {
            runner_error(
                RunnerErrorKind::Unavailable,
                false,
                "could not start Bubblewrap",
            )
        })?;
        let stdout = child.stdout.take().ok_or_else(|| {
            runner_error(
                RunnerErrorKind::Internal,
                false,
                "stdout pipe was unavailable",
            )
        })?;
        let stderr = child.stderr.take().ok_or_else(|| {
            runner_error(
                RunnerErrorKind::Internal,
                false,
                "stderr pipe was unavailable",
            )
        })?;
        let output_budget = Arc::new(SharedOutputBudget {
            used: AtomicU64::new(0),
            maximum: request.budget.max_output_bytes,
        });
        let stdout_task = tokio::spawn(capture_output(stdout, output_budget.clone()));
        let stderr_task = tokio::spawn(capture_output(stderr, output_budget));
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
        let output_bytes = (stdout_capture.bytes.len() + stderr_capture.bytes.len()) as u64;
        let output_truncated = stdout_capture.truncated || stderr_capture.truncated;
        let stdout_text = String::from_utf8_lossy(&stdout_capture.bytes).into_owned();
        let stderr_text = String::from_utf8_lossy(&stderr_capture.bytes).into_owned();
        let exit_code = exit_status.and_then(|status| status.code());

        // A timeout wins over a simultaneous output-limit/exit event because the
        // wall-clock budget is the outer execution boundary.
        let timed_out = timed_out || elapsed_ms > request.budget.max_wall_time_ms;
        let (mut disposition, mut failure, mut event_type) = if timed_out {
            (ExecutionDisposition::TimedOut, None, "execution_timed_out")
        } else if output_limit_exceeded {
            (
                ExecutionDisposition::Errored,
                Some(RunnerFailure {
                    code: "output_limit_exceeded".into(),
                    message: "process output exceeded the configured capture limit; sandbox was terminated".into(),
                    retryable: false,
                }),
                "execution_errored",
            )
        } else if drain_timed_out {
            (
                ExecutionDisposition::Errored,
                Some(RunnerFailure {
                    code: "output_drain_timed_out".into(),
                    message: "process output did not close within the drain grace period after the process exited; unread output was discarded".into(),
                    retryable: false,
                }),
                "execution_errored",
            )
        } else if exit_code == Some(0) {
            (ExecutionDisposition::Finished, None, "execution_finished")
        } else {
            (
                ExecutionDisposition::Errored,
                Some(RunnerFailure {
                    code: "process_exit_nonzero".into(),
                    message: "the allowlisted process exited unsuccessfully".into(),
                    retryable: false,
                }),
                "execution_errored",
            )
        };

        // Collect /workspace/output only when the process ended on its own:
        // then the kernel has already killed and reaped everything in the
        // sandbox's PID namespace, so nothing can change the files while the
        // host reads them. After a kill, PID 1 dies asynchronously, so the
        // files are discarded instead.
        let mut artifacts = Vec::new();
        let mut artifact_bytes = 0_u64;
        let mut output_files = Vec::new();
        let mut rejected_outputs = Vec::new();
        let export = match &output_staging {
            None => "not_requested".to_string(),
            Some(staging) if timed_out || output_limit_exceeded || drain_timed_out => {
                staging.discard();
                "skipped_after_termination".to_string()
            }
            Some(staging) => match collect_outputs(
                &staging.partial,
                request.budget.max_artifact_bytes,
                self.max_output_files,
            ) {
                Ok(collected) if collected.limit_exceeded.is_some() => {
                    staging.discard();
                    let reason = collected.limit_exceeded.unwrap_or_default();
                    disposition = ExecutionDisposition::Errored;
                    event_type = "execution_errored";
                    failure = Some(RunnerFailure {
                        code: "artifact_limit_exceeded".into(),
                        message: format!("workspace outputs were discarded: {reason}"),
                        retryable: false,
                    });
                    "discarded_over_limit".to_string()
                }
                Ok(collected) => {
                    staging.publish()?;
                    for file in &collected.files {
                        artifacts.push(ArtifactManifest {
                            artifact_id: file.path.clone(),
                            content_type: "application/octet-stream".into(),
                            byte_length: file.byte_length,
                            sha256_hex: file.sha256_hex.clone(),
                            redaction_state: RedactionState::NotRequired,
                        });
                        output_files.push(json!({
                            "path": file.path,
                            "byte_length": file.byte_length,
                            "sha256": file.sha256_hex,
                        }));
                    }
                    for (path, reason) in &collected.rejected {
                        rejected_outputs.push(json!({"path": path, "reason": reason}));
                    }
                    artifact_bytes = collected.total_bytes;
                    "collected".to_string()
                }
                Err(_) => {
                    staging.discard();
                    disposition = ExecutionDisposition::Errored;
                    event_type = "execution_errored";
                    failure = Some(RunnerFailure {
                        code: "artifact_collection_failed".into(),
                        message: "workspace outputs could not be read; they were discarded".into(),
                        retryable: false,
                    });
                    "failed".to_string()
                }
            },
        };

        let ended_at = Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true);
        let executable_id = task.executable_id.clone();
        let output = json!({
            "executable_id": executable_id,
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
            "rejected_output_entries": rejected_outputs,
        });

        // Keep the input directory alive until execution and output collection end.
        drop(input_directory);
        Ok(RunnerResult {
            request_id: request.request_id.clone(),
            disposition,
            events: vec![
                RunnerEvent {
                    sequence: 1,
                    occurred_at: started_at,
                    actor: "local_process_runner".into(),
                    event_type: "execution_started".into(),
                    payload: json!({ "executable_id": executable_id }),
                },
                RunnerEvent {
                    sequence: 2,
                    occurred_at: ended_at,
                    actor: "local_process_runner".into(),
                    event_type: event_type.into(),
                    payload: json!({
                        "exit_code": exit_code,
                        "output_truncated": output_truncated,
                        "timed_out": timed_out,
                    }),
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

impl Runner for LocalProcessRunner {
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

pub(crate) struct CollectedOutput {
    pub(crate) exit_status: Option<ExitStatus>,
    pub(crate) stdout: OutputCapture,
    pub(crate) stderr: OutputCapture,
    pub(crate) timed_out: bool,
    pub(crate) output_limit_exceeded: bool,
    /// A pipe stayed open past `drain_grace` after the process exited; that
    /// stream's capture was abandoned and is reported as empty and truncated.
    pub(crate) drain_timed_out: bool,
}

/// Waits for the process and both output captures, enforcing the wall-time
/// budget and the shared output limit. Once the process has exited, the
/// captures get at most `drain_grace` to finish, so a descendant that keeps a
/// pipe open can never stall the runner past its budget.
pub(crate) async fn collect_process_output(
    child: &mut tokio::process::Child,
    mut stdout_task: tokio::task::JoinHandle<io::Result<OutputCapture>>,
    mut stderr_task: tokio::task::JoinHandle<io::Result<OutputCapture>>,
    wall_time: Duration,
    drain_grace: Duration,
) -> Result<CollectedOutput, RunnerError> {
    let mut stdout_capture: Option<OutputCapture> = None;
    let mut stderr_capture: Option<OutputCapture> = None;
    let mut exit_status: Option<ExitStatus> = None;
    let mut timed_out = false;
    let mut output_limit_exceeded = false;
    let mut drain_timed_out = false;
    let deadline = tokio::time::sleep(wall_time);
    tokio::pin!(deadline);
    let drain_deadline = tokio::time::sleep(drain_grace);
    tokio::pin!(drain_deadline);
    let mut drain_armed = false;

    loop {
        if exit_status.is_some() && stdout_capture.is_some() && stderr_capture.is_some() {
            break;
        }
        if exit_status.is_some() && !drain_armed {
            drain_deadline
                .as_mut()
                .reset(tokio::time::Instant::now() + drain_grace);
            drain_armed = true;
        }
        tokio::select! {
            status = child.wait(), if exit_status.is_none() => {
                exit_status = Some(status.map_err(|_| runner_error(RunnerErrorKind::Internal, false, "could not wait for the sandbox process"))?);
            }
            result = &mut stdout_task, if stdout_capture.is_none() => {
                let captured = finish_capture(result)?;
                if captured.truncated && !output_limit_exceeded {
                    output_limit_exceeded = true;
                    let _ = child.kill().await;
                    if exit_status.is_none() {
                        exit_status = Some(child.wait().await.map_err(|_| runner_error(RunnerErrorKind::Internal, false, "could not stop an output-limited process"))?);
                    }
                }
                stdout_capture = Some(captured);
            }
            result = &mut stderr_task, if stderr_capture.is_none() => {
                let captured = finish_capture(result)?;
                if captured.truncated && !output_limit_exceeded {
                    output_limit_exceeded = true;
                    let _ = child.kill().await;
                    if exit_status.is_none() {
                        exit_status = Some(child.wait().await.map_err(|_| runner_error(RunnerErrorKind::Internal, false, "could not stop an output-limited process"))?);
                    }
                }
                stderr_capture = Some(captured);
            }
            _ = &mut deadline, if !timed_out => {
                timed_out = true;
                let _ = child.kill().await;
                if exit_status.is_none() {
                    exit_status = Some(child.wait().await.map_err(|_| runner_error(RunnerErrorKind::Internal, false, "could not stop a timed-out process"))?);
                }
            }
            _ = &mut drain_deadline, if drain_armed => {
                drain_timed_out = true;
                if stdout_capture.is_none() {
                    stdout_task.abort();
                    stdout_capture = Some(OutputCapture { bytes: Vec::new(), truncated: true });
                }
                if stderr_capture.is_none() {
                    stderr_task.abort();
                    stderr_capture = Some(OutputCapture { bytes: Vec::new(), truncated: true });
                }
            }
        }
    }

    match (stdout_capture, stderr_capture) {
        (Some(stdout), Some(stderr)) => Ok(CollectedOutput {
            exit_status,
            stdout,
            stderr,
            timed_out,
            output_limit_exceeded,
            drain_timed_out,
        }),
        _ => Err(runner_error(
            RunnerErrorKind::Internal,
            false,
            "output capture loop ended without both captures",
        )),
    }
}

pub(crate) async fn capture_output<R>(
    mut reader: R,
    budget: Arc<SharedOutputBudget>,
) -> io::Result<OutputCapture>
where
    R: tokio::io::AsyncRead + Unpin + Send + 'static,
{
    let mut bytes = Vec::new();
    let mut buffer = [0_u8; 8192];
    loop {
        let read = reader.read(&mut buffer).await?;
        if read == 0 {
            return Ok(OutputCapture {
                bytes,
                truncated: false,
            });
        }
        let accepted = budget.reserve(read);
        bytes.extend_from_slice(&buffer[..accepted]);
        if accepted < read {
            return Ok(OutputCapture {
                bytes,
                truncated: true,
            });
        }
    }
}

fn finish_capture(
    result: Result<io::Result<OutputCapture>, tokio::task::JoinError>,
) -> Result<OutputCapture, RunnerError> {
    result
        .map_err(|_| {
            runner_error(
                RunnerErrorKind::Internal,
                false,
                "output capture task failed",
            )
        })?
        .map_err(|_| {
            runner_error(
                RunnerErrorKind::Internal,
                false,
                "could not read sandbox process output",
            )
        })
}

fn preflight_bubblewrap(bubblewrap_path: &Path) -> Result<(), RunnerError> {
    let true_path = Path::new("/usr/bin/true");
    if !true_path.is_file() {
        return Err(unavailable(
            "/usr/bin/true is required for the Bubblewrap preflight",
        ));
    }
    let mut command = StdCommand::new(bubblewrap_path);
    command.env_clear().current_dir("/");
    append_bwrap_prefix_std(&mut command);
    command
        .arg("--size")
        .arg("1048576")
        .arg("--tmpfs")
        .arg("/tmp")
        .arg("--size")
        .arg("1048576")
        .arg("--tmpfs")
        .arg("/workspace")
        .arg("--chdir")
        .arg("/workspace")
        .arg("--")
        .arg(true_path);
    let output = command
        .output()
        .map_err(|_| unavailable("could not execute Bubblewrap preflight"))?;
    if !output.status.success() {
        let detail = String::from_utf8_lossy(&output.stderr);
        let concise = detail.chars().take(240).collect::<String>();
        return Err(runner_error(
            RunnerErrorKind::UnsupportedPolicy,
            false,
            format!(
                "Bubblewrap namespace preflight failed; refusing unsandboxed fallback: {concise}"
            ),
        ));
    }
    Ok(())
}

fn append_bwrap_prefix(command: &mut Command) {
    command
        .arg("--die-with-parent")
        .arg("--new-session")
        .arg("--unshare-user")
        .arg("--unshare-pid")
        .arg("--unshare-ipc")
        .arg("--unshare-uts")
        .arg("--unshare-net")
        .arg("--cap-drop")
        .arg("ALL")
        .arg("--ro-bind")
        .arg("/usr")
        .arg("/usr");
    append_system_aliases(command);
    command.arg("--dev").arg("/dev").arg("--proc").arg("/proc");
}

fn append_bwrap_prefix_std(command: &mut StdCommand) {
    command
        .arg("--die-with-parent")
        .arg("--new-session")
        .arg("--unshare-user")
        .arg("--unshare-pid")
        .arg("--unshare-ipc")
        .arg("--unshare-uts")
        .arg("--unshare-net")
        .arg("--cap-drop")
        .arg("ALL")
        .arg("--ro-bind")
        .arg("/usr")
        .arg("/usr");
    append_system_aliases_std(command);
    command.arg("--dev").arg("/dev").arg("--proc").arg("/proc");
}

fn append_system_aliases(command: &mut Command) {
    for (alias, target, is_symlink) in system_aliases() {
        if is_symlink {
            command.arg("--symlink").arg(target).arg(alias);
        } else {
            command.arg("--ro-bind").arg(&alias).arg(&alias);
        }
    }
}

fn append_system_aliases_std(command: &mut StdCommand) {
    for (alias, target, is_symlink) in system_aliases() {
        if is_symlink {
            command.arg("--symlink").arg(target).arg(alias);
        } else {
            command.arg("--ro-bind").arg(&alias).arg(&alias);
        }
    }
}

fn system_aliases() -> Vec<(String, PathBuf, bool)> {
    let mut aliases = Vec::new();
    for alias in ["/bin", "/sbin", "/lib", "/lib64"] {
        let path = Path::new(alias);
        let Ok(metadata) = fs::symlink_metadata(path) else {
            continue;
        };
        if metadata.file_type().is_symlink() {
            if let Ok(target) = fs::read_link(path) {
                aliases.push((alias.to_string(), target, true));
            }
        } else if metadata.is_dir() {
            aliases.push((alias.to_string(), path.to_path_buf(), false));
        }
    }
    aliases
}

pub(crate) fn write_input_files(
    root: &Path,
    files: &[LocalProcessInputFile],
    maximum_bytes: u64,
) -> Result<u64, RunnerError> {
    let mut total_bytes = 0_u64;
    for item in files {
        validate_relative_input_path(&item.path)?;
        total_bytes = total_bytes
            .checked_add(item.content.len() as u64)
            .ok_or_else(|| invalid("input file byte count overflow"))?;
        if total_bytes > maximum_bytes {
            return Err(invalid(
                "input files exceed the configured aggregate byte limit",
            ));
        }
        let destination = root.join(Path::new(&item.path));
        let parent = destination
            .parent()
            .ok_or_else(|| invalid("input file path has no parent"))?;
        fs::create_dir_all(parent)
            .map_err(|_| invalid("could not create input file parent directory"))?;
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options
            .open(&destination)
            .map_err(|_| invalid("input file paths must be unique and non-conflicting"))?;
        file.write_all(item.content.as_bytes()).map_err(|_| {
            runner_error(
                RunnerErrorKind::Internal,
                false,
                "could not write an input file",
            )
        })?;
        file.sync_all().map_err(|_| {
            runner_error(
                RunnerErrorKind::Internal,
                false,
                "could not flush an input file",
            )
        })?;
    }
    Ok(total_bytes)
}

pub(crate) fn create_temporary_directory(
    root: &Path,
) -> Result<TemporaryInputDirectory, RunnerError> {
    for _ in 0..32 {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let sequence = TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let path = root.join(format!(
            "cybercore-input-{}-{now}-{sequence}",
            std::process::id()
        ));
        match fs::create_dir(&path) {
            Ok(()) => {
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    if fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).is_err() {
                        let _ = fs::remove_dir_all(&path);
                        return Err(runner_error(
                            RunnerErrorKind::Internal,
                            false,
                            "could not secure temporary input directory",
                        ));
                    }
                }
                return Ok(TemporaryInputDirectory { path });
            }
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(_) => {
                return Err(runner_error(
                    RunnerErrorKind::Internal,
                    false,
                    "could not create temporary input directory",
                ))
            }
        }
    }
    Err(runner_error(
        RunnerErrorKind::Internal,
        false,
        "could not allocate a unique temporary input directory",
    ))
}

pub(crate) fn validate_requested_budgets(
    requested_wall_time_ms: u64,
    requested_output_bytes: u64,
    maximum_wall_time_ms: u64,
    maximum_output_bytes: u64,
) -> Result<(), RunnerError> {
    if requested_wall_time_ms == 0
        || requested_output_bytes == 0
        || requested_wall_time_ms > maximum_wall_time_ms
        || requested_output_bytes > maximum_output_bytes
    {
        return Err(invalid(
            "requested wall-time or output budget is outside this runner's configured limits",
        ));
    }
    Ok(())
}

fn validate_relative_input_path(value: &str) -> Result<(), RunnerError> {
    if value.len() > MAX_INPUT_PATH_BYTES
        || value.trim().is_empty()
        || value.contains('\\')
        || value.contains('\0')
        || value.contains(':')
        || value.split('/').any(|part| {
            part.is_empty()
                || part == "."
                || part == ".."
                || part.len() > MAX_INPUT_PATH_COMPONENT_BYTES
        })
    {
        return Err(invalid("input file paths must be non-empty relative POSIX paths without dot, parent, or empty components"));
    }
    let path = Path::new(value);
    if path.is_absolute()
        || !path
            .components()
            .all(|component| matches!(component, Component::Normal(_)))
    {
        return Err(invalid(
            "input file paths must not contain absolute, dot, or parent components",
        ));
    }
    Ok(())
}

pub(crate) fn valid_logical_id(value: &str) -> bool {
    !value.is_empty()
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
}

pub(crate) fn valid_environment_name(value: &str) -> bool {
    let mut bytes = value.bytes();
    let Some(first) = bytes.next() else {
        return false;
    };
    (first.is_ascii_alphabetic() || first == b'_')
        && bytes.all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
}

pub(crate) fn is_reserved_environment_name(value: &str) -> bool {
    matches!(
        value,
        "PATH" | "HOME" | "PWD" | "TMPDIR" | "IFS" | "ENV" | "BASH_ENV" | "SHELLOPTS" | "CDPATH"
    ) || value.starts_with("LD_")
        || value.starts_with("DYLD_")
        || matches!(
            value,
            "GCONV_PATH" | "PYTHONPATH" | "PYTHONHOME" | "NODE_OPTIONS"
        )
}

#[cfg(unix)]
fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    fs::metadata(path)
        .map(|metadata| metadata.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

#[cfg(not(unix))]
fn is_executable(_path: &Path) -> bool {
    false
}

pub(crate) fn runner_error(
    kind: RunnerErrorKind,
    retryable: bool,
    message: impl Into<String>,
) -> RunnerError {
    RunnerError {
        kind,
        retryable,
        message: message.into(),
    }
}

pub(crate) fn invalid(message: impl Into<String>) -> RunnerError {
    runner_error(RunnerErrorKind::InvalidRequest, false, message)
}

pub(crate) fn unavailable(message: impl Into<String>) -> RunnerError {
    runner_error(RunnerErrorKind::Unavailable, false, message)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{duplex, AsyncWriteExt};

    #[test]
    fn input_paths_reject_traversal_and_absolute_paths() {
        assert!(validate_relative_input_path("src/main.rs").is_ok());
        assert!(validate_relative_input_path("../secret").is_err());
        assert!(validate_relative_input_path("/etc/passwd").is_err());
        assert!(validate_relative_input_path("a/./b").is_err());
        assert!(validate_relative_input_path("a\\b").is_err());
        assert!(validate_relative_input_path(&"a".repeat(MAX_INPUT_PATH_BYTES + 1)).is_err());
        assert!(
            validate_relative_input_path(&"a".repeat(MAX_INPUT_PATH_COMPONENT_BYTES + 1)).is_err()
        );
    }

    #[test]
    fn request_budgets_cannot_exceed_runner_capabilities() {
        assert!(validate_requested_budgets(1_000, 1_000, 2_000, 2_000).is_ok());
        assert!(validate_requested_budgets(0, 1_000, 2_000, 2_000).is_err());
        assert!(validate_requested_budgets(1_000, 0, 2_000, 2_000).is_err());
        assert!(validate_requested_budgets(2_001, 1_000, 2_000, 2_000).is_err());
        assert!(validate_requested_budgets(1_000, 2_001, 2_000, 2_000).is_err());
    }

    #[test]
    fn environment_names_require_allowlist_safe_syntax() {
        assert!(valid_environment_name("APP_MODE"));
        assert!(!valid_environment_name("1APP"));
        assert!(is_reserved_environment_name("LD_PRELOAD"));
        assert!(is_reserved_environment_name("PATH"));
        assert!(!is_reserved_environment_name("APP_MODE"));
    }

    #[test]
    fn logical_executable_ids_are_stable_and_restricted() {
        assert!(valid_logical_id("python3"));
        assert!(valid_logical_id("agent.cli-v2"));
        assert!(!valid_logical_id("../python"));
        assert!(!valid_logical_id("python 3"));
    }

    #[test]
    fn task_input_rejects_unknown_properties() {
        let value = json!({
            "executable_id": "python3",
            "args": ["-V"],
            "unexpected": true
        });
        assert!(serde_json::from_value::<LocalProcessTaskInput>(value).is_err());
    }

    #[tokio::test]
    async fn output_capture_never_retains_more_than_the_shared_budget() {
        let (mut writer, reader) = duplex(32);
        let budget = Arc::new(SharedOutputBudget {
            used: AtomicU64::new(0),
            maximum: 5,
        });
        let writer_task = tokio::spawn(async move {
            writer.write_all(b"abcdefghij").await.unwrap();
        });
        let captured = capture_output(reader, budget.clone()).await.unwrap();
        writer_task.await.unwrap();
        assert_eq!(captured.bytes, b"abcde");
        assert!(captured.truncated);
        assert_eq!(budget.used.load(Ordering::Relaxed), 5);
    }

    #[tokio::test]
    async fn stdout_and_stderr_share_one_output_budget() {
        let (mut stdout_writer, stdout_reader) = duplex(32);
        let (mut stderr_writer, stderr_reader) = duplex(32);
        let budget = Arc::new(SharedOutputBudget {
            used: AtomicU64::new(0),
            maximum: 7,
        });
        let stdout_budget = budget.clone();
        let stderr_budget = budget.clone();
        let stdout_task = tokio::spawn(async move {
            stdout_writer.write_all(b"stdout").await.unwrap();
            drop(stdout_writer);
            capture_output(stdout_reader, stdout_budget).await.unwrap()
        });
        let stderr_task = tokio::spawn(async move {
            stderr_writer.write_all(b"stderr").await.unwrap();
            drop(stderr_writer);
            capture_output(stderr_reader, stderr_budget).await.unwrap()
        });
        let (stdout, stderr) = tokio::join!(stdout_task, stderr_task);
        let stdout = stdout.unwrap();
        let stderr = stderr.unwrap();
        assert!(stdout.bytes.len() + stderr.bytes.len() <= 7);
        assert_eq!(budget.used.load(Ordering::Relaxed), 7);
        assert!(stdout.truncated || stderr.truncated);
    }

    #[test]
    fn duplicate_input_paths_fail_closed() {
        let root = std::env::temp_dir().join(format!(
            "cybercore-local-runner-test-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        let temp = create_temporary_directory(&root).unwrap();
        let files = vec![
            LocalProcessInputFile {
                path: "input.txt".into(),
                content: "one".into(),
            },
            LocalProcessInputFile {
                path: "input.txt".into(),
                content: "two".into(),
            },
        ];
        assert!(write_input_files(&temp.path, &files, 1024).is_err());
        drop(temp);
        let _ = fs::remove_dir_all(root);
    }

    /// Runs `script` under the host `sh` (no sandbox) through the same
    /// collection loop the Bubblewrap runner uses.
    async fn collect_host_script(
        script: &str,
        wall_time: Duration,
        drain_grace: Duration,
    ) -> (CollectedOutput, Duration) {
        let mut child = Command::new("/bin/sh")
            .arg("-c")
            .arg(script)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        let budget = Arc::new(SharedOutputBudget {
            used: AtomicU64::new(0),
            maximum: 1024,
        });
        let stdout = tokio::spawn(capture_output(child.stdout.take().unwrap(), budget.clone()));
        let stderr = tokio::spawn(capture_output(child.stderr.take().unwrap(), budget));
        let started = Instant::now();
        let collected = collect_process_output(&mut child, stdout, stderr, wall_time, drain_grace)
            .await
            .unwrap();
        (collected, started.elapsed())
    }

    #[tokio::test]
    async fn normal_exit_collects_output_without_drain_timeout() {
        let (collected, _) = collect_host_script(
            "printf out; printf err >&2",
            Duration::from_secs(10),
            Duration::from_millis(500),
        )
        .await;
        assert_eq!(collected.stdout.bytes, b"out");
        assert_eq!(collected.stderr.bytes, b"err");
        assert_eq!(collected.exit_status.and_then(|s| s.code()), Some(0));
        assert!(!collected.timed_out && !collected.output_limit_exceeded);
        assert!(!collected.drain_timed_out);
    }

    #[tokio::test]
    async fn pipe_held_open_after_exit_is_bounded_by_drain_grace() {
        // The background `sleep` inherits stdout/stderr and keeps both pipes
        // open after the shell itself exits.
        let (collected, elapsed) = collect_host_script(
            "sleep 3 & exit 0",
            Duration::from_secs(10),
            Duration::from_millis(200),
        )
        .await;
        assert!(elapsed < Duration::from_secs(2), "took {elapsed:?}");
        assert_eq!(collected.exit_status.and_then(|s| s.code()), Some(0));
        assert!(!collected.timed_out);
        assert!(collected.drain_timed_out);
        assert!(collected.stdout.truncated && collected.stderr.truncated);
    }

    #[tokio::test]
    async fn pipe_held_open_after_timeout_kill_is_bounded_by_drain_grace() {
        // Killing the shell at the deadline leaves the background `sleep`
        // holding the pipes; before the drain grace this waited for it.
        let (collected, elapsed) = collect_host_script(
            "sleep 3 & sleep 30",
            Duration::from_millis(200),
            Duration::from_millis(200),
        )
        .await;
        assert!(elapsed < Duration::from_secs(2), "took {elapsed:?}");
        assert!(collected.timed_out);
        assert!(collected.drain_timed_out);
    }
}
