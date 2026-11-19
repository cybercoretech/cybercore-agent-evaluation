//! The evaluation file `workbench run` reads.

use cybercore_evaluation_core::{
    ContainerEgressConfig, ContainerRunner, ContainerRunnerConfig, EvaluationSchedule,
    LocalProcessRunner, LocalProcessRunnerConfig, NetworkPolicy, RecordReference, Runner,
    RunnerError,
};
use cybercore_evaluation_judge::{Inspect, JudgeCriterion, ModelJudgeConfig, ModelJudgeVerifier};
use cybercore_evaluation_store::{
    Check, ChecksVerifier, CompositeVerifier, CriterionCheck, StdoutEqualsVerifier, Verifier,
};
use serde::Deserialize;
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

pub const FORMAT: &str = "cybercore-workbench-evaluation/v0";

/// One evaluation: the frozen schedule, how to run it, and how to verify it.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EvaluationFile {
    /// Must be `cybercore-workbench-evaluation/v0`.
    pub format: String,
    pub schedule: EvaluationSchedule,
    pub runner: RunnerSpec,
    /// Hidden grader data. Never part of any trial's `task_input`.
    pub verifier: VerifierSpec,
    /// `project_ref` for evidence artifacts.
    pub project_ref: RecordReference,
    /// Store each runner output as evidence. The local runner does not redact,
    /// so only enable this for non-sensitive tasks.
    #[serde(default)]
    pub store_output_evidence: bool,
    /// `task_revision` records stored before the run. Their acceptance
    /// criteria (criticality) and required criteria set each run's outcome
    /// rules; trials whose task revision is absent use the default rules.
    #[serde(default)]
    pub task_revisions: Vec<Value>,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum RunnerSpec {
    /// The Linux Bubblewrap local process runner (prototype isolation; see
    /// docs/architecture/LOCAL-PROCESS-RUNNER.md).
    LocalBubblewrap {
        #[serde(default = "default_runner_id")]
        runner_id: String,
        #[serde(default = "default_bubblewrap")]
        bubblewrap_path: PathBuf,
        /// Logical executable IDs → absolute host paths.
        allowed_executables: BTreeMap<String, PathBuf>,
        #[serde(default)]
        allowed_environment_keys: BTreeSet<String>,
        #[serde(default)]
        limits: RunnerLimits,
    },
    /// Rootless Podman containers with cgroup memory/CPU/process limits, no
    /// network and a read-only image (ADR-0015). Images must already be
    /// pulled; the runner never pulls.
    Podman {
        #[serde(default = "default_podman_runner_id")]
        runner_id: String,
        #[serde(default = "default_podman")]
        podman_path: PathBuf,
        /// Logical image IDs → local image references (pinned to image IDs at start).
        images: BTreeMap<String, String>,
        /// Logical executable IDs → absolute paths inside the images.
        allowed_executables: BTreeMap<String, String>,
        #[serde(default)]
        allowed_environment_keys: BTreeSet<String>,
        #[serde(default)]
        limits: RunnerLimits,
        #[serde(default)]
        resources: ContainerResources,
        /// Allowlisted network access for trials whose `network_policy` is an
        /// allowlist (ADR-0019). Absent: every trial must deny all network.
        #[serde(default)]
        egress: Option<EgressSpec>,
    },
}

/// Host egress proxy settings for the Podman runner.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EgressSpec {
    /// A statically linked `cybercore-egress-forwarder`; defaults to the one
    /// next to the `workbench` binary.
    #[serde(default)]
    pub forwarder_path: Option<PathBuf>,
    #[serde(default = "default_egress_ports")]
    pub ports: BTreeSet<u16>,
    #[serde(default = "default_egress_connections")]
    pub max_connections: usize,
    #[serde(default = "default_egress_bytes")]
    pub max_bytes: u64,
    /// Allow names resolving to loopback/private addresses. Testing only.
    #[serde(default)]
    pub allow_private_destinations: bool,
    /// Container variable name → where its secret value comes from. Values
    /// are never written in the file.
    #[serde(default)]
    pub credentials: BTreeMap<String, CredentialSource>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CredentialSource {
    /// Host environment variable read when the run starts.
    pub from_env: String,
}

fn default_egress_ports() -> BTreeSet<u16> {
    BTreeSet::from([443])
}

fn default_egress_connections() -> usize {
    64
}

fn default_egress_bytes() -> u64 {
    64 * 1024 * 1024
}

fn trial_credentials(task_input: &Value) -> Vec<&str> {
    task_input
        .get("credentials")
        .and_then(Value::as_array)
        .map(|names| names.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default()
}

/// Per-container cgroup limits.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct ContainerResources {
    pub memory_bytes: u64,
    /// Thousandths of a CPU (1000 = one CPU).
    pub cpu_millis: u32,
    pub pids_limit: u32,
}

impl Default for ContainerResources {
    fn default() -> Self {
        Self {
            memory_bytes: 512 * 1024 * 1024,
            cpu_millis: 1000,
            pids_limit: 256,
        }
    }
}

fn default_podman_runner_id() -> String {
    "workbench-podman".into()
}

fn default_podman() -> PathBuf {
    PathBuf::from("/usr/bin/podman")
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct RunnerLimits {
    pub max_wall_time_ms: u64,
    pub max_output_bytes: u64,
    pub max_workspace_bytes: u64,
    pub max_tmp_bytes: u64,
    pub max_input_file_bytes: u64,
    pub max_input_files: usize,
    pub max_arguments: usize,
    pub max_argument_bytes: u64,
    /// Ceiling for a trial's `max_artifact_bytes` (collected output files).
    pub max_artifact_bytes: u64,
    /// Most files one run may leave in `/workspace/output`.
    pub max_output_files: usize,
}

impl Default for RunnerLimits {
    fn default() -> Self {
        Self {
            max_wall_time_ms: 60_000,
            max_output_bytes: 64 * 1024,
            max_workspace_bytes: 64 * 1024 * 1024,
            max_tmp_bytes: 16 * 1024 * 1024,
            max_input_file_bytes: 4 * 1024 * 1024,
            max_input_files: 64,
            max_arguments: 256,
            max_argument_bytes: 64 * 1024,
            max_artifact_bytes: 16 * 1024 * 1024,
            max_output_files: 256,
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum VerifierSpec {
    /// Pass when the captured stdout equals the expected text for the trial.
    StdoutEquals { expected: BTreeMap<String, String> },
    /// Several criteria per trial over stdout and the files a trial with
    /// `task_input.collect_outputs` leaves in `/workspace/output`.
    Checks {
        checks: BTreeMap<String, Vec<CheckSpec>>,
    },
    /// A Claude model judges each criterion's rubric (ADR-0017). Judged
    /// outputs are sent to the Anthropic API, so this requires explicit
    /// consent; credentials come only from ANTHROPIC_API_KEY or
    /// ANTHROPIC_AUTH_TOKEN.
    ModelJudge {
        consent_to_send_outputs: bool,
        #[serde(default)]
        model: Option<String>,
        #[serde(default)]
        effort: Option<String>,
        #[serde(default)]
        max_input_bytes: Option<usize>,
        criteria: BTreeMap<String, Vec<JudgeCriterionSpec>>,
    },
    /// Several verifiers in one evaluation, e.g. exact `checks` for files plus
    /// a `model_judge` for rubric criteria (ADR-0018). Criterion IDs must be
    /// unique per trial across the listed verifiers; no nesting.
    Composite { verifiers: Vec<VerifierSpec> },
}

/// One judged criterion. Without `file`, the judge reads stdout.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct JudgeCriterionSpec {
    pub criterion_id: String,
    /// What "pass" means, in plain language (hidden from the agent).
    pub rubric: String,
    /// A collected `/workspace/output` file to judge instead of stdout.
    #[serde(default)]
    pub file: Option<String>,
}

const EFFORTS: &[&str] = &["low", "medium", "high", "xhigh", "max"];

/// One criterion in a `checks` verifier. Paths are relative to
/// `/workspace/output`.
#[derive(Debug, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum CheckSpec {
    StdoutEquals {
        criterion_id: String,
        expected: String,
    },
    StdoutContains {
        criterion_id: String,
        text: String,
    },
    FileExists {
        criterion_id: String,
        path: String,
    },
    FileAbsent {
        criterion_id: String,
        path: String,
    },
    FileEquals {
        criterion_id: String,
        path: String,
        expected: String,
    },
    FileContains {
        criterion_id: String,
        path: String,
        text: String,
    },
}

impl CheckSpec {
    fn criterion_id(&self) -> &str {
        match self {
            Self::StdoutEquals { criterion_id, .. }
            | Self::StdoutContains { criterion_id, .. }
            | Self::FileExists { criterion_id, .. }
            | Self::FileAbsent { criterion_id, .. }
            | Self::FileEquals { criterion_id, .. }
            | Self::FileContains { criterion_id, .. } => criterion_id,
        }
    }

    fn path(&self) -> Option<&str> {
        match self {
            Self::FileExists { path, .. }
            | Self::FileAbsent { path, .. }
            | Self::FileEquals { path, .. }
            | Self::FileContains { path, .. } => Some(path),
            Self::StdoutEquals { .. } | Self::StdoutContains { .. } => None,
        }
    }

    fn to_check(&self) -> CriterionCheck {
        let check = match self {
            Self::StdoutEquals { expected, .. } => Check::StdoutEquals {
                expected: expected.clone(),
            },
            Self::StdoutContains { text, .. } => Check::StdoutContains { text: text.clone() },
            Self::FileExists { path, .. } => Check::FileExists { path: path.clone() },
            Self::FileAbsent { path, .. } => Check::FileAbsent { path: path.clone() },
            Self::FileEquals { path, expected, .. } => Check::FileEquals {
                path: path.clone(),
                expected: expected.clone(),
            },
            Self::FileContains { path, text, .. } => Check::FileContains {
                path: path.clone(),
                text: text.clone(),
            },
        };
        CriterionCheck {
            criterion_id: self.criterion_id().to_string(),
            check,
        }
    }
}

/// `[A-Za-z0-9][A-Za-z0-9._:-]{0,63}`: becomes part of record IDs.
fn valid_criterion_id(id: &str) -> bool {
    let mut chars = id.chars();
    chars.next().is_some_and(|c| c.is_ascii_alphanumeric())
        && id.len() <= 64
        && chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | ':' | '-'))
}

/// A relative path made only of normal components (no `..`, no root).
fn valid_output_path(path: &str) -> bool {
    !path.is_empty()
        && Path::new(path)
            .components()
            .all(|c| matches!(c, std::path::Component::Normal(_)))
}

fn default_runner_id() -> String {
    "workbench-local-bwrap".into()
}

fn default_bubblewrap() -> PathBuf {
    PathBuf::from("/usr/bin/bwrap")
}

impl EvaluationFile {
    /// Reads and checks a file. Errors name the file and the offending field.
    pub fn load(path: &Path) -> Result<Self, String> {
        let text = std::fs::read_to_string(path)
            .map_err(|error| format!("{}: cannot read: {error}", path.display()))?;
        let file: Self =
            serde_json::from_str(&text).map_err(|error| format!("{}: {error}", path.display()))?;
        file.check()
            .map_err(|message| format!("{}: {message}", path.display()))?;
        Ok(file)
    }

    fn check(&self) -> Result<(), String> {
        if self.format != FORMAT {
            return Err(format!(
                "format must be {FORMAT:?}, found {:?}",
                self.format
            ));
        }
        let executables: BTreeSet<&str> = match &self.runner {
            RunnerSpec::LocalBubblewrap {
                allowed_executables,
                bubblewrap_path,
                ..
            } => {
                if !bubblewrap_path.is_absolute() {
                    return Err("runner.bubblewrap_path must be absolute".into());
                }
                for (id, path) in allowed_executables {
                    if !path.is_absolute() {
                        return Err(format!(
                            "runner.allowed_executables.{id} must be an absolute path"
                        ));
                    }
                }
                allowed_executables.keys().map(String::as_str).collect()
            }
            RunnerSpec::Podman {
                podman_path,
                images,
                allowed_executables,
                ..
            } => {
                if !podman_path.is_absolute() {
                    return Err("runner.podman_path must be absolute".into());
                }
                if images.is_empty() {
                    return Err("runner.images must list at least one image".into());
                }
                for (id, path) in allowed_executables {
                    if !path.starts_with('/') {
                        return Err(format!(
                            "runner.allowed_executables.{id} must be an absolute path inside the image"
                        ));
                    }
                }
                for trial in &self.schedule.trials {
                    let id = &trial.trial_ref.record_id;
                    match trial.task_input.get("image_id").and_then(Value::as_str) {
                        Some(image) if images.contains_key(image) => {}
                        Some(image) => {
                            return Err(format!(
                                "schedule.trials[{id}].task_input.image_id {image:?} is not in runner.images"
                            ))
                        }
                        None if images.len() == 1 => {}
                        None => {
                            return Err(format!(
                                "schedule.trials[{id}].task_input.image_id is required when runner.images lists several images"
                            ))
                        }
                    }
                }
                allowed_executables.keys().map(String::as_str).collect()
            }
        };
        if executables.is_empty() {
            return Err("runner.allowed_executables must list at least one executable".into());
        }
        for trial in &self.schedule.trials {
            let id = &trial.trial_ref.record_id;
            match trial.task_input.get("executable_id").and_then(Value::as_str) {
                Some(executable) if executables.contains(executable) => {}
                Some(executable) => {
                    return Err(format!(
                        "schedule.trials[{id}].task_input.executable_id {executable:?} is not in runner.allowed_executables"
                    ))
                }
                None => {
                    return Err(format!(
                        "schedule.trials[{id}].task_input.executable_id is required by the runner"
                    ))
                }
            }
        }
        let egress = match &self.runner {
            RunnerSpec::Podman { egress, .. } => egress.as_ref(),
            RunnerSpec::LocalBubblewrap { .. } => None,
        };
        if let Some(egress) = egress {
            if egress.ports.is_empty() || egress.ports.contains(&0) {
                return Err("runner.egress.ports must list at least one non-zero port".into());
            }
            for (name, source) in &egress.credentials {
                if source.from_env.is_empty() || source.from_env.contains(['=', '\0']) {
                    return Err(format!(
                        "runner.egress.credentials.{name}.from_env must name an environment variable"
                    ));
                }
            }
        }
        for trial in &self.schedule.trials {
            let id = &trial.trial_ref.record_id;
            let allowlisted = match &trial.network_policy {
                NetworkPolicy::DenyAll => false,
                NetworkPolicy::Allowlist { hosts } => {
                    if egress.is_none() {
                        return Err(format!(
                            "schedule.trials[{id}].network_policy is an allowlist, which needs runner.type \"podman\" with runner.egress"
                        ));
                    }
                    if hosts.is_empty() {
                        return Err(format!(
                            "schedule.trials[{id}].network_policy.hosts must list at least one host"
                        ));
                    }
                    true
                }
            };
            let credentials = trial_credentials(&trial.task_input);
            if !credentials.is_empty() && !allowlisted {
                return Err(format!(
                    "schedule.trials[{id}].task_input.credentials need an allowlist network_policy"
                ));
            }
            for name in credentials {
                if !egress.is_some_and(|e| e.credentials.contains_key(name)) {
                    return Err(format!(
                        "schedule.trials[{id}].task_input.credentials lists {name:?}, which is not in runner.egress.credentials"
                    ));
                }
            }
        }
        let task_ids: BTreeSet<&str> = self
            .schedule
            .trials
            .iter()
            .map(|t| t.task_revision_ref.record_id.as_str())
            .collect();
        for (index, record) in self.task_revisions.iter().enumerate() {
            if record["record_type"] != "task_revision" {
                return Err(format!(
                    "task_revisions[{index}].record_type must be \"task_revision\""
                ));
            }
            let id = record["record_id"].as_str().unwrap_or("");
            if !task_ids.contains(id) {
                return Err(format!(
                    "task_revisions[{index}].record_id {id:?} is not the task_revision_ref of any trial"
                ));
            }
        }
        let limits = self.limits();
        for trial in &self.schedule.trials {
            if collects_outputs(trial) {
                let id = &trial.trial_ref.record_id;
                let budget = trial.budget.max_artifact_bytes;
                if budget == 0 || budget > limits.max_artifact_bytes {
                    return Err(format!(
                        "schedule.trials[{id}].budget.max_artifact_bytes must be between 1 and runner.limits.max_artifact_bytes ({}) when task_input.collect_outputs is true",
                        limits.max_artifact_bytes
                    ));
                }
            }
        }
        self.check_verifier(&self.verifier, "verifier", true)?;
        Ok(())
    }

    /// Validates one verifier spec; `at_root` is its field path for errors.
    fn check_verifier(&self, spec: &VerifierSpec, at_root: &str, top: bool) -> Result<(), String> {
        let trial_ids: BTreeSet<&str> = self
            .schedule
            .trials
            .iter()
            .map(|t| t.trial_ref.record_id.as_str())
            .collect();
        match spec {
            VerifierSpec::Composite { verifiers } => {
                if !top {
                    return Err(format!(
                        "{at_root}: a composite verifier cannot contain another composite"
                    ));
                }
                if verifiers.is_empty() {
                    return Err(format!(
                        "{at_root}.verifiers must list at least one verifier"
                    ));
                }
                for (index, sub) in verifiers.iter().enumerate() {
                    self.check_verifier(sub, &format!("{at_root}.verifiers[{index}]"), false)?;
                }
                let mut seen: BTreeMap<(String, String), usize> = BTreeMap::new();
                for (index, sub) in verifiers.iter().enumerate() {
                    for key in criterion_ids(sub) {
                        if let Some(first) = seen.insert(key.clone(), index) {
                            return Err(format!(
                                "{at_root}.verifiers[{first}] and {at_root}.verifiers[{index}] both judge criterion {:?} for trial {}; give each verifier its own criterion IDs",
                                key.1, key.0
                            ));
                        }
                    }
                }
            }
            VerifierSpec::StdoutEquals { expected } => {
                if let Some(unknown) = expected.keys().find(|k| !trial_ids.contains(k.as_str())) {
                    return Err(format!(
                        "{at_root}.expected.{unknown} does not name a trial in the schedule"
                    ));
                }
            }
            VerifierSpec::ModelJudge {
                consent_to_send_outputs,
                effort,
                max_input_bytes,
                criteria,
                ..
            } => {
                if !consent_to_send_outputs {
                    return Err(format!("{at_root}.consent_to_send_outputs must be true: the judged outputs are sent to the Anthropic API"));
                }
                if let Some(effort) = effort {
                    if !EFFORTS.contains(&effort.as_str()) {
                        return Err(format!(
                            "{at_root}.effort {effort:?} must be one of low, medium, high, xhigh, max"
                        ));
                    }
                }
                if *max_input_bytes == Some(0) {
                    return Err(format!("{at_root}.max_input_bytes must be positive"));
                }
                for (trial_id, specs) in criteria {
                    let Some(trial) = self
                        .schedule
                        .trials
                        .iter()
                        .find(|t| &t.trial_ref.record_id == trial_id)
                    else {
                        return Err(format!(
                            "{at_root}.criteria.{trial_id} does not name a trial in the schedule"
                        ));
                    };
                    let mut seen = BTreeSet::new();
                    for (index, spec) in specs.iter().enumerate() {
                        let at = format!("{at_root}.criteria.{trial_id}[{index}]");
                        if !valid_criterion_id(&spec.criterion_id) {
                            return Err(format!(
                                "{at}.criterion_id {:?} must match [A-Za-z0-9][A-Za-z0-9._:-]{{0,63}}",
                                spec.criterion_id
                            ));
                        }
                        if !seen.insert(spec.criterion_id.as_str()) {
                            return Err(format!(
                                "{at}.criterion_id {:?} is repeated",
                                spec.criterion_id
                            ));
                        }
                        if spec.rubric.trim().is_empty() {
                            return Err(format!("{at}.rubric must not be empty"));
                        }
                        if let Some(path) = &spec.file {
                            if !valid_output_path(path) {
                                return Err(format!(
                                    "{at}.file {path:?} must be relative to /workspace/output without '..'"
                                ));
                            }
                            if !collects_outputs(trial) {
                                return Err(format!(
                                    "{at} judges a file, but schedule.trials[{trial_id}].task_input.collect_outputs is not true"
                                ));
                            }
                        }
                    }
                }
            }
            VerifierSpec::Checks { checks } => {
                for (trial_id, specs) in checks {
                    let Some(trial) = self
                        .schedule
                        .trials
                        .iter()
                        .find(|t| &t.trial_ref.record_id == trial_id)
                    else {
                        return Err(format!(
                            "{at_root}.checks.{trial_id} does not name a trial in the schedule"
                        ));
                    };
                    let mut seen = BTreeSet::new();
                    for (index, spec) in specs.iter().enumerate() {
                        let at = format!("{at_root}.checks.{trial_id}[{index}]");
                        let criterion = spec.criterion_id();
                        if !valid_criterion_id(criterion) {
                            return Err(format!(
                                "{at}.criterion_id {criterion:?} must match [A-Za-z0-9][A-Za-z0-9._:-]{{0,63}}"
                            ));
                        }
                        if !seen.insert(criterion) {
                            return Err(format!("{at}.criterion_id {criterion:?} is repeated"));
                        }
                        if let Some(path) = spec.path() {
                            if !valid_output_path(path) {
                                return Err(format!(
                                    "{at}.path {path:?} must be relative to /workspace/output without '..'"
                                ));
                            }
                            if !collects_outputs(trial) {
                                return Err(format!(
                                    "{at} checks a file, but schedule.trials[{trial_id}].task_input.collect_outputs is not true"
                                ));
                            }
                        }
                    }
                }
            }
        }
        Ok(())
    }

    pub fn evaluation_id(&self) -> &str {
        &self.schedule.evaluation_ref.record_id
    }

    /// Trials the verifier has no expectation for (they will be recorded as
    /// verification `unavailable`, never as passing).
    pub fn unverifiable_trials(&self) -> Vec<&str> {
        let configured = |id: &str| spec_covers(&self.verifier, id);
        self.schedule
            .trials
            .iter()
            .map(|t| t.trial_ref.record_id.as_str())
            .filter(|id| !configured(id))
            .collect()
    }

    /// One line describing the verifier for `--dry-run`.
    pub fn verifier_summary(&self) -> String {
        summary(&self.verifier)
    }

    /// True when any trial asks the runner to collect `/workspace/output`.
    pub fn collects_outputs(&self) -> bool {
        self.schedule.trials.iter().any(collects_outputs)
    }

    fn limits(&self) -> &RunnerLimits {
        match &self.runner {
            RunnerSpec::LocalBubblewrap { limits, .. } | RunnerSpec::Podman { limits, .. } => {
                limits
            }
        }
    }

    /// Builds the declared runner. `artifact_root` is used only when some
    /// trial collects outputs.
    pub fn build_runner(
        &self,
        temporary_root: PathBuf,
        artifact_root: PathBuf,
    ) -> Result<Box<dyn Runner>, RunnerError> {
        let artifact_root = self.collects_outputs().then_some(artifact_root);
        match &self.runner {
            RunnerSpec::LocalBubblewrap {
                runner_id,
                bubblewrap_path,
                allowed_executables,
                allowed_environment_keys,
                limits,
            } => Ok(Box::new(LocalProcessRunner::new(
                LocalProcessRunnerConfig {
                    runner_id: runner_id.clone(),
                    runner_version: env!("CARGO_PKG_VERSION").into(),
                    bubblewrap_path: bubblewrap_path.clone(),
                    temporary_root,
                    allowed_executables: allowed_executables.clone(),
                    allowed_environment_keys: allowed_environment_keys.clone(),
                    max_wall_time_ms: limits.max_wall_time_ms,
                    max_output_bytes: limits.max_output_bytes,
                    max_workspace_bytes: limits.max_workspace_bytes,
                    max_tmp_bytes: limits.max_tmp_bytes,
                    max_input_file_bytes: limits.max_input_file_bytes,
                    max_input_files: limits.max_input_files,
                    max_arguments: limits.max_arguments,
                    max_argument_bytes: limits.max_argument_bytes,
                    artifact_root,
                    max_artifact_bytes: limits.max_artifact_bytes,
                    max_output_files: limits.max_output_files,
                },
            )?)),
            RunnerSpec::Podman {
                runner_id,
                podman_path,
                images,
                allowed_executables,
                allowed_environment_keys,
                limits,
                resources,
                egress,
            } => {
                let runner = ContainerRunner::new(ContainerRunnerConfig {
                    runner_id: runner_id.clone(),
                    runner_version: env!("CARGO_PKG_VERSION").into(),
                    podman_path: podman_path.clone(),
                    temporary_root,
                    images: images.clone(),
                    allowed_executables: allowed_executables.clone(),
                    allowed_environment_keys: allowed_environment_keys.clone(),
                    max_wall_time_ms: limits.max_wall_time_ms,
                    max_output_bytes: limits.max_output_bytes,
                    memory_bytes: resources.memory_bytes,
                    cpu_millis: resources.cpu_millis,
                    pids_limit: resources.pids_limit,
                    workspace_bytes: limits.max_workspace_bytes,
                    tmp_bytes: limits.max_tmp_bytes,
                    max_input_file_bytes: limits.max_input_file_bytes,
                    max_input_files: limits.max_input_files,
                    max_arguments: limits.max_arguments,
                    max_argument_bytes: limits.max_argument_bytes,
                    artifact_root,
                    max_artifact_bytes: limits.max_artifact_bytes,
                    max_output_files: limits.max_output_files,
                })?;
                match egress {
                    None => Ok(Box::new(runner)),
                    Some(egress) => Ok(Box::new(runner.with_egress(egress_config(egress)?)?)),
                }
            }
        }
    }

    pub fn verifier(&self) -> Box<dyn Verifier> {
        build_verifier(&self.verifier)
    }
}

fn config_error(message: impl Into<String>) -> RunnerError {
    RunnerError {
        kind: cybercore_evaluation_core::RunnerErrorKind::InvalidRequest,
        retryable: false,
        message: message.into(),
    }
}

/// Resolves the forwarder path and reads granted secrets from the host
/// environment. A missing variable is an error, never an empty secret.
fn egress_config(spec: &EgressSpec) -> Result<ContainerEgressConfig, RunnerError> {
    let forwarder_path = match &spec.forwarder_path {
        Some(path) => path.clone(),
        None => std::env::current_exe()
            .ok()
            .and_then(|exe| exe.parent().map(|dir| dir.join("cybercore-egress-forwarder")))
            .ok_or_else(|| config_error("cannot locate the workbench binary to find cybercore-egress-forwarder; set runner.egress.forwarder_path"))?,
    };
    let mut credentials = BTreeMap::new();
    for (name, source) in &spec.credentials {
        let value = std::env::var(&source.from_env).map_err(|_| {
            config_error(format!(
                "runner.egress.credentials.{name} reads host environment variable {}, which is not set",
                source.from_env
            ))
        })?;
        credentials.insert(name.clone(), value);
    }
    Ok(ContainerEgressConfig {
        forwarder_path,
        ports: spec.ports.clone(),
        max_connections: spec.max_connections,
        max_bytes: spec.max_bytes,
        allow_private_destinations: spec.allow_private_destinations,
        credentials,
    })
}

fn build_verifier(spec: &VerifierSpec) -> Box<dyn Verifier> {
    match spec {
        VerifierSpec::Composite { verifiers } => Box::new(CompositeVerifier::new(
            verifiers.iter().map(build_verifier).collect(),
        )),
        VerifierSpec::StdoutEquals { expected } => {
            Box::new(StdoutEqualsVerifier::new(expected.clone()))
        }
        VerifierSpec::ModelJudge {
            model,
            effort,
            max_input_bytes,
            criteria,
            ..
        } => {
            let criteria = criteria
                .iter()
                .map(|(trial, specs)| {
                    let judged = specs
                        .iter()
                        .map(|spec| JudgeCriterion {
                            criterion_id: spec.criterion_id.clone(),
                            rubric: spec.rubric.clone(),
                            inspect: match &spec.file {
                                Some(path) => Inspect::File(path.clone()),
                                None => Inspect::Stdout,
                            },
                        })
                        .collect();
                    (trial.clone(), judged)
                })
                .collect();
            let mut config = ModelJudgeConfig::new(criteria);
            if let Some(model) = model {
                config = config.with_model(model.clone());
            }
            if let Some(effort) = effort {
                config.effort = effort.clone();
            }
            if let Some(limit) = max_input_bytes {
                config.max_input_bytes = *limit;
            }
            Box::new(ModelJudgeVerifier::new(config))
        }
        VerifierSpec::Checks { checks } => Box::new(ChecksVerifier::new(
            checks
                .iter()
                .map(|(trial, specs)| {
                    (
                        trial.clone(),
                        specs.iter().map(CheckSpec::to_check).collect(),
                    )
                })
                .collect(),
        )),
    }
}

fn spec_covers(spec: &VerifierSpec, trial_id: &str) -> bool {
    match spec {
        VerifierSpec::StdoutEquals { expected } => expected.contains_key(trial_id),
        VerifierSpec::Checks { checks } => checks.get(trial_id).is_some_and(|c| !c.is_empty()),
        VerifierSpec::ModelJudge { criteria, .. } => {
            criteria.get(trial_id).is_some_and(|c| !c.is_empty())
        }
        VerifierSpec::Composite { verifiers } => verifiers.iter().any(|v| spec_covers(v, trial_id)),
    }
}

fn summary(spec: &VerifierSpec) -> String {
    match spec {
        VerifierSpec::StdoutEquals { .. } => "stdout_equals (deterministic)".into(),
        VerifierSpec::Checks { .. } => "checks (deterministic)".into(),
        VerifierSpec::ModelJudge { model, effort, .. } => format!(
            "model_judge ({}, effort {}): judged outputs are sent to the Anthropic API",
            model
                .as_deref()
                .unwrap_or(cybercore_evaluation_judge::DEFAULT_MODEL),
            effort
                .as_deref()
                .unwrap_or(cybercore_evaluation_judge::DEFAULT_EFFORT)
        ),
        VerifierSpec::Composite { verifiers } => format!(
            "composite of {}",
            verifiers
                .iter()
                .map(summary)
                .collect::<Vec<_>>()
                .join(" + ")
        ),
    }
}

/// (trial ID, criterion ID) pairs a verifier spec can produce.
fn criterion_ids(spec: &VerifierSpec) -> Vec<(String, String)> {
    match spec {
        VerifierSpec::StdoutEquals { expected } => expected
            .keys()
            .map(|t| (t.clone(), "stdout-equals-expected".to_string()))
            .collect(),
        VerifierSpec::Checks { checks } => checks
            .iter()
            .flat_map(|(t, specs)| {
                specs
                    .iter()
                    .map(move |c| (t.clone(), c.criterion_id().to_string()))
            })
            .collect(),
        VerifierSpec::ModelJudge { criteria, .. } => criteria
            .iter()
            .flat_map(|(t, specs)| {
                specs
                    .iter()
                    .map(move |c| (t.clone(), c.criterion_id.clone()))
            })
            .collect(),
        VerifierSpec::Composite { verifiers } => verifiers.iter().flat_map(criterion_ids).collect(),
    }
}

fn collects_outputs(trial: &cybercore_evaluation_core::TrialDefinition) -> bool {
    trial
        .task_input
        .get("collect_outputs")
        .and_then(Value::as_bool)
        == Some(true)
}
