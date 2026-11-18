//! The evaluation file `workbench run` reads.

use cybercore_evaluation_core::{EvaluationSchedule, LocalProcessRunnerConfig, RecordReference};
use cybercore_evaluation_store::StdoutEqualsVerifier;
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
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum VerifierSpec {
    /// Pass when the captured stdout equals the expected text for the trial.
    StdoutEquals { expected: BTreeMap<String, String> },
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
        let VerifierSpec::StdoutEquals { expected } = &self.verifier;
        let trial_ids: BTreeSet<&str> = self
            .schedule
            .trials
            .iter()
            .map(|t| t.trial_ref.record_id.as_str())
            .collect();
        if let Some(unknown) = expected.keys().find(|k| !trial_ids.contains(k.as_str())) {
            return Err(format!(
                "verifier.expected.{unknown} does not name a trial in the schedule"
            ));
        }
        Ok(())
    }

    pub fn evaluation_id(&self) -> &str {
        &self.schedule.evaluation_ref.record_id
    }

    /// Trials the verifier has no expectation for (they will be recorded as
    /// verification `unavailable`, never as passing).
    pub fn unverifiable_trials(&self) -> Vec<&str> {
        let VerifierSpec::StdoutEquals { expected } = &self.verifier;
        self.schedule
            .trials
            .iter()
            .map(|t| t.trial_ref.record_id.as_str())
            .filter(|id| !expected.contains_key(*id))
            .collect()
    }

    pub fn runner_config(&self, temporary_root: PathBuf) -> LocalProcessRunnerConfig {
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
            // File checks in evaluation files are not wired up yet.
            artifact_root: None,
            max_artifact_bytes: 0,
            max_output_files: 0,
        }
    }

    pub fn verifier(&self) -> StdoutEqualsVerifier {
        let VerifierSpec::StdoutEquals { expected } = &self.verifier;
        StdoutEqualsVerifier::new(expected.clone())
    }
}
