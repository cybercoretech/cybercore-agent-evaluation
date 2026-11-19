//! The evaluation file `workbench run` reads.

use cybercore_evaluation_core::{EvaluationSchedule, LocalProcessRunnerConfig, RecordReference};
use cybercore_evaluation_store::{
    Check, ChecksVerifier, CriterionCheck, StdoutEqualsVerifier, Verifier,
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
}

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
        let RunnerSpec::LocalBubblewrap {
            allowed_executables,
            bubblewrap_path,
            ..
        } = &self.runner;
        if !bubblewrap_path.is_absolute() {
            return Err("runner.bubblewrap_path must be absolute".into());
        }
        if allowed_executables.is_empty() {
            return Err("runner.allowed_executables must list at least one executable".into());
        }
        for (id, path) in allowed_executables {
            if !path.is_absolute() {
                return Err(format!(
                    "runner.allowed_executables.{id} must be an absolute path"
                ));
            }
        }
        for trial in &self.schedule.trials {
            let id = &trial.trial_ref.record_id;
            match trial.task_input.get("executable_id").and_then(Value::as_str) {
                Some(executable) if allowed_executables.contains_key(executable) => {}
                Some(executable) => {
                    return Err(format!(
                        "schedule.trials[{id}].task_input.executable_id {executable:?} is not in runner.allowed_executables"
                    ))
                }
                None => {
                    return Err(format!(
                        "schedule.trials[{id}].task_input.executable_id is required by the local_bubblewrap runner"
                    ))
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
        let RunnerSpec::LocalBubblewrap { limits, .. } = &self.runner;
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
        let trial_ids: BTreeSet<&str> = self
            .schedule
            .trials
            .iter()
            .map(|t| t.trial_ref.record_id.as_str())
            .collect();
        match &self.verifier {
            VerifierSpec::StdoutEquals { expected } => {
                if let Some(unknown) = expected.keys().find(|k| !trial_ids.contains(k.as_str())) {
                    return Err(format!(
                        "verifier.expected.{unknown} does not name a trial in the schedule"
                    ));
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
                            "verifier.checks.{trial_id} does not name a trial in the schedule"
                        ));
                    };
                    let mut seen = BTreeSet::new();
                    for (index, spec) in specs.iter().enumerate() {
                        let at = format!("verifier.checks.{trial_id}[{index}]");
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
        let configured = |id: &str| match &self.verifier {
            VerifierSpec::StdoutEquals { expected } => expected.contains_key(id),
            VerifierSpec::Checks { checks } => checks.get(id).is_some_and(|c| !c.is_empty()),
        };
        self.schedule
            .trials
            .iter()
            .map(|t| t.trial_ref.record_id.as_str())
            .filter(|id| !configured(id))
            .collect()
    }

    /// True when any trial asks the runner to collect `/workspace/output`.
    pub fn collects_outputs(&self) -> bool {
        self.schedule.trials.iter().any(collects_outputs)
    }

    /// `artifact_root` is used only when some trial collects outputs.
    pub fn runner_config(
        &self,
        temporary_root: PathBuf,
        artifact_root: PathBuf,
    ) -> LocalProcessRunnerConfig {
        let RunnerSpec::LocalBubblewrap {
            runner_id,
            bubblewrap_path,
            allowed_executables,
            allowed_environment_keys,
            limits,
        } = &self.runner;
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
            artifact_root: self.collects_outputs().then_some(artifact_root),
            max_artifact_bytes: limits.max_artifact_bytes,
            max_output_files: limits.max_output_files,
        }
    }

    pub fn verifier(&self) -> Box<dyn Verifier> {
        match &self.verifier {
            VerifierSpec::StdoutEquals { expected } => {
                Box::new(StdoutEqualsVerifier::new(expected.clone()))
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
}

fn collects_outputs(trial: &cybercore_evaluation_core::TrialDefinition) -> bool {
    trial
        .task_input
        .get("collect_outputs")
        .and_then(Value::as_bool)
        == Some(true)
}
