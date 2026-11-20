//! Task suites (ADR-0024): tasks and agents described once, expanded into
//! one ordinary evaluation file per agent.
//!
//! Expansion is pure and deterministic: the same suite always gives the same
//! evaluation files, so running a suite again resumes the same evaluations.
//! Each expanded file then goes through exactly the validation a hand-written
//! evaluation file gets, so `workbench expand` output and `workbench run
//! suite.json` always agree.

use crate::evaluation_file::{RunnerLimits, FORMAT};
use serde::Deserialize;
use serde_json::{json, Map, Value};
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

pub const SUITE_FORMAT: &str = "cybercore-workbench-suite/v0";
const MAX_REPEATS: u32 = 100;
const MAX_ID: usize = 40;
/// `created_at` of generated task revisions unless the suite sets one, so
/// expansion stays deterministic.
const DEFAULT_CREATED_AT: &str = "2026-11-19T00:00:00Z";
const CHECKS_VERIFIER: &str = "VERIFIER-SUITE-CHECKS";

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Suite {
    pub format: String,
    pub suite_id: String,
    /// How many times every agent runs every task.
    #[serde(default = "one")]
    pub repeats: u32,
    /// Shared by every agent, exactly as in an evaluation file.
    pub runner: Value,
    #[serde(default)]
    pub defaults: Defaults,
    pub agents: Vec<AgentSpec>,
    pub tasks: Vec<TaskSpec>,
    #[serde(default)]
    pub project_ref: Option<Value>,
    #[serde(default)]
    pub store_output_evidence: bool,
    /// As an evaluation file's `schedule.retry_policy`; a sensible default
    /// retries runner errors once.
    #[serde(default)]
    pub retry_policy: Option<Value>,
    #[serde(default)]
    pub pricing: Option<Value>,
    /// Settings for tasks' `judge` rubrics (the model judge, ADR-0017).
    #[serde(default)]
    pub judge: Option<JudgeSettings>,
    /// Comparisons frozen in advance in each treatment's plan (ADR-0021).
    #[serde(default)]
    pub compare: Vec<CompareSpec>,
    /// The question the comparisons answer. Defaults to a pass-rate question.
    #[serde(default)]
    pub decision_question: Option<String>,
    /// `created_at` for the generated task revisions (RFC 3339).
    #[serde(default)]
    pub created_at: Option<String>,
    /// Readiness gates (ADR-0028), declared in every agent's evaluation and
    /// frozen with its plan.
    #[serde(default)]
    pub readiness: Option<Value>,
}

fn one() -> u32 {
    1
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Defaults {
    #[serde(default)]
    pub budget: BudgetSpec,
    /// Collect `/workspace/output` from every trial (needed for file checks
    /// and usage reports).
    #[serde(default)]
    pub collect_outputs: bool,
    /// Attempts that run at once (default 2).
    #[serde(default)]
    pub max_active_runs: Option<u32>,
}

/// Any field left out comes from the suite defaults, then the runner limits.
#[derive(Debug, Default, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BudgetSpec {
    pub max_wall_time_ms: Option<u64>,
    pub max_output_bytes: Option<u64>,
    pub max_artifact_bytes: Option<u64>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentSpec {
    pub agent_id: String,
    pub executable_id: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub environment: BTreeMap<String, String>,
    #[serde(default)]
    pub image_id: Option<String>,
    /// Allowlisted hosts (Podman runner with `egress`); none means no network.
    #[serde(default)]
    pub network: Option<NetworkSpec>,
    #[serde(default)]
    pub credentials: Vec<String>,
    /// System fingerprint ID; defaults to `FP-<suite>-<agent>`.
    #[serde(default)]
    pub fingerprint: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NetworkSpec {
    pub hosts: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskSpec {
    pub task_id: String,
    pub objective: String,
    /// What the agent is told; defaults to the objective. Recorded on the
    /// task revision only: pass it to the agent through `files` or
    /// `environment`.
    #[serde(default)]
    pub instruction: Option<String>,
    #[serde(default)]
    pub family: Option<String>,
    #[serde(default)]
    pub difficulty: Option<String>,
    /// Input files, as a trial's `task_input.files`.
    #[serde(default)]
    pub files: Vec<Value>,
    #[serde(default)]
    pub environment: BTreeMap<String, String>,
    /// Exact criteria, as a `checks` verifier's entries.
    #[serde(default)]
    pub checks: Vec<Value>,
    /// Rubric criteria for the model judge.
    #[serde(default)]
    pub judge: Vec<Value>,
    #[serde(default)]
    pub budget: BudgetSpec,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct JudgeSettings {
    pub consent_to_send_outputs: bool,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub effort: Option<String>,
    #[serde(default)]
    pub max_input_bytes: Option<usize>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CompareSpec {
    pub baseline: String,
    pub treatment: String,
    #[serde(default)]
    pub comparison_id: Option<String>,
    #[serde(default)]
    pub alpha: Option<f64>,
    #[serde(default)]
    pub minimum_matched_tasks: Option<u64>,
    #[serde(default)]
    pub practical_significance_threshold: Option<f64>,
}

/// One expanded evaluation: its ID and its evaluation-file JSON.
pub struct Expanded {
    pub evaluation_id: String,
    pub file: Value,
}

/// True when the JSON text is a suite rather than an evaluation file.
pub fn is_suite(text: &str) -> bool {
    serde_json::from_str::<Value>(text)
        .ok()
        .and_then(|v| v.get("format").and_then(Value::as_str).map(str::to_string))
        .as_deref()
        == Some(SUITE_FORMAT)
}

fn valid_id(id: &str) -> bool {
    let bytes = id.as_bytes();
    !bytes.is_empty()
        && bytes.len() <= MAX_ID
        && bytes[0].is_ascii_alphanumeric()
        && bytes
            .iter()
            .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(b))
}

fn check_id(id: &str, at: &str) -> Result<(), String> {
    if valid_id(id) {
        Ok(())
    } else {
        Err(format!(
            "{at} must be 1 to {MAX_ID} letters, digits, '.', '_' or '-', starting with a letter or digit"
        ))
    }
}

impl Suite {
    pub fn load(path: &Path) -> Result<Self, String> {
        let text = std::fs::read_to_string(path)
            .map_err(|error| format!("{}: cannot read: {error}", path.display()))?;
        let suite: Self =
            serde_json::from_str(&text).map_err(|error| format!("{}: {error}", path.display()))?;
        suite
            .check()
            .map_err(|message| format!("{}: {message}", path.display()))?;
        Ok(suite)
    }

    /// Checks what only the suite can know; everything else is checked on
    /// the expanded evaluation files.
    fn check(&self) -> Result<(), String> {
        if self.format != SUITE_FORMAT {
            return Err(format!(
                "format must be {SUITE_FORMAT:?}, found {:?}",
                self.format
            ));
        }
        check_id(&self.suite_id, "suite_id")?;
        if self.repeats == 0 || self.repeats > MAX_REPEATS {
            return Err(format!("repeats must be between 1 and {MAX_REPEATS}"));
        }
        if self.agents.is_empty() {
            return Err("agents must list at least one agent".into());
        }
        if self.tasks.is_empty() {
            return Err("tasks must list at least one task".into());
        }
        let mut agents = BTreeSet::new();
        for agent in &self.agents {
            check_id(&agent.agent_id, "agents[].agent_id")?;
            if !agents.insert(agent.agent_id.as_str()) {
                return Err(format!(
                    "agents[{}] is listed more than once",
                    agent.agent_id
                ));
            }
            if agent.network.is_none() && !agent.credentials.is_empty() {
                return Err(format!(
                    "agents[{}].credentials need agents[{}].network (credentials are only granted with an allowlist)",
                    agent.agent_id, agent.agent_id
                ));
            }
            if let Some(network) = &agent.network {
                if network.hosts.is_empty() {
                    return Err(format!(
                        "agents[{}].network.hosts must list at least one host",
                        agent.agent_id
                    ));
                }
            }
        }
        let mut tasks = BTreeSet::new();
        for task in &self.tasks {
            check_id(&task.task_id, "tasks[].task_id")?;
            let at = format!("tasks[{}]", task.task_id);
            if !tasks.insert(task.task_id.as_str()) {
                return Err(format!("{at} is listed more than once"));
            }
            if task.objective.trim().is_empty() {
                return Err(format!("{at}.objective must not be empty"));
            }
            if task.checks.is_empty() && task.judge.is_empty() {
                return Err(format!("{at} needs at least one entry in checks or judge"));
            }
            if !task.judge.is_empty() && self.judge.is_none() {
                return Err(format!(
                    "{at}.judge needs the suite's judge settings (judge.consent_to_send_outputs)"
                ));
            }
            let mut criteria = BTreeSet::new();
            for (index, criterion) in task.checks.iter().chain(&task.judge).enumerate() {
                let id = criterion
                    .get("criterion_id")
                    .and_then(Value::as_str)
                    .ok_or_else(|| format!("{at}: criterion {index} needs a criterion_id"))?;
                if !criteria.insert(id) {
                    return Err(format!("{at}: criterion_id {id:?} is used more than once"));
                }
            }
            for agent in &self.agents {
                if let Some(key) = agent
                    .environment
                    .keys()
                    .find(|k| task.environment.contains_key(*k))
                {
                    return Err(format!(
                        "{at}.environment.{key} is also set by agents[{}]; use distinct names",
                        agent.agent_id
                    ));
                }
            }
        }
        let mut declared = BTreeSet::new();
        for (index, c) in self.compare.iter().enumerate() {
            let at = format!("compare[{index}]");
            for (side, id) in [("baseline", &c.baseline), ("treatment", &c.treatment)] {
                if !agents.contains(id.as_str()) {
                    return Err(format!("{at}.{side} {id:?} is not an agent_id"));
                }
            }
            if c.baseline == c.treatment {
                return Err(format!("{at} compares {:?} with itself", c.baseline));
            }
            if !declared.insert((c.baseline.as_str(), c.treatment.as_str())) {
                return Err(format!("{at} repeats an earlier comparison"));
            }
        }
        Ok(())
    }

    pub fn evaluation_id(&self, agent_id: &str) -> String {
        format!("{}-{agent_id}", self.suite_id)
    }

    fn task_revision_id(&self, task_id: &str) -> String {
        format!("TASK-{}-{task_id}", self.suite_id)
    }

    fn budget(&self, task: &TaskSpec) -> Value {
        let defaults = RunnerLimits::default();
        let limit =
            |key: &str, fallback: u64| self.runner["limits"][key].as_u64().unwrap_or(fallback);
        let collect = self.defaults.collect_outputs;
        let pick = |task: Option<u64>, suite: Option<u64>, fallback: u64| {
            task.or(suite).unwrap_or(fallback)
        };
        let base = &self.defaults.budget;
        json!({
            "max_wall_time_ms": pick(
                task.budget.max_wall_time_ms,
                base.max_wall_time_ms,
                limit("max_wall_time_ms", defaults.max_wall_time_ms),
            ),
            "max_output_bytes": pick(
                task.budget.max_output_bytes,
                base.max_output_bytes,
                limit("max_output_bytes", defaults.max_output_bytes),
            ),
            "max_artifact_bytes": if collect {
                pick(
                    task.budget.max_artifact_bytes,
                    base.max_artifact_bytes,
                    limit("max_artifact_bytes", defaults.max_artifact_bytes),
                )
            } else {
                0
            },
            "max_tool_calls": 0,
            "max_input_tokens": null,
            "max_output_tokens": null,
        })
    }

    fn task_revision(&self, task: &TaskSpec) -> Value {
        let criteria: Vec<Value> = task
            .checks
            .iter()
            .map(|check| {
                let kind = check["kind"].as_str().unwrap_or("check").replace('_', " ");
                let target = check["path"]
                    .as_str()
                    .map(|p| format!(" on {p}"))
                    .unwrap_or_default();
                (
                    check,
                    format!("Exact check: {kind}{target}."),
                    "Checked exactly by the workbench's checks verifier.",
                )
            })
            .chain(task.judge.iter().map(|criterion| {
                (
                    criterion,
                    "Judged against a hidden rubric by the model judge.".to_string(),
                    "Judged by the model judge against the rubric in the evaluation's verifier.",
                )
            }))
            .map(|(criterion, description, requirement)| {
                json!({
                    "criterion_id": criterion["criterion_id"],
                    "description": description,
                    "verification_requirement": requirement,
                    "criticality": "mandatory",
                })
            })
            .collect();
        let ids: Vec<&Value> = criteria.iter().map(|c| &c["criterion_id"]).collect();
        let mut record = json!({
            "record_id": self.task_revision_id(&task.task_id),
            "record_type": "task_revision",
            "schema_version": "0.1.0",
            "created_at": self.created_at.as_deref().unwrap_or(DEFAULT_CREATED_AT),
            "provenance": {
                "source": {
                    "system": "workbench-suite",
                    "component": SUITE_FORMAT,
                    "component_version": "0.1.0",
                    "origin_id": self.suite_id,
                },
                "captured_by": "workbench-suite",
                "capture_version": "0.1.0",
            },
            "benchmark_ref": {"record_id": format!("BENCH-{}", self.suite_id), "record_type": "benchmark_revision"},
            "task_key": task.task_id,
            "revision": 1,
            "validity_state": "valid",
            "task_family": task.family.as_deref().unwrap_or("default"),
            "objective": task.objective,
            "agent_instruction": task.instruction.as_deref().unwrap_or(&task.objective),
            "acceptance_criteria": criteria,
            "verification_plan": {
                "required_criterion_ids": ids,
                "verifier_refs": [{"record_id": CHECKS_VERIFIER, "record_type": "verifier"}],
                "independent_verification_required": true,
            },
        });
        if let (Some(difficulty), Some(map)) = (&task.difficulty, record.as_object_mut()) {
            map.insert("difficulty_stratum".into(), json!(difficulty));
        }
        record
    }

    fn verifier(&self, agent: &AgentSpec) -> Value {
        let mut checks = Map::new();
        let mut judged = Map::new();
        for task in &self.tasks {
            for repeat in 1..=self.repeats {
                let trial = self.trial_id(agent, task, repeat);
                if !task.checks.is_empty() {
                    checks.insert(trial.clone(), json!(task.checks));
                }
                if !task.judge.is_empty() {
                    judged.insert(trial, json!(task.judge));
                }
            }
        }
        let checks = json!({"type": "checks", "checks": checks});
        match (&self.judge, judged.is_empty()) {
            (Some(settings), false) => {
                let mut judge = json!({
                    "type": "model_judge",
                    "consent_to_send_outputs": settings.consent_to_send_outputs,
                    "criteria": judged,
                });
                if let Some(map) = judge.as_object_mut() {
                    if let Some(model) = &settings.model {
                        map.insert("model".into(), json!(model));
                    }
                    if let Some(effort) = &settings.effort {
                        map.insert("effort".into(), json!(effort));
                    }
                    if let Some(bytes) = settings.max_input_bytes {
                        map.insert("max_input_bytes".into(), json!(bytes));
                    }
                }
                if checks["checks"].as_object().is_some_and(Map::is_empty) {
                    judge
                } else {
                    json!({"type": "composite", "verifiers": [checks, judge]})
                }
            }
            _ => checks,
        }
    }

    fn trial_id(&self, agent: &AgentSpec, task: &TaskSpec, repeat: u32) -> String {
        format!(
            "TRIAL-{}-{}-{}-R{repeat}",
            self.suite_id, agent.agent_id, task.task_id
        )
    }

    fn runner(&self) -> Value {
        // Every environment name the suite sets must be allowed by the runner.
        let mut runner = self.runner.clone();
        let mut keys: BTreeSet<String> = runner["allowed_environment_keys"]
            .as_array()
            .map(|keys| {
                keys.iter()
                    .filter_map(Value::as_str)
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default();
        keys.extend(
            self.agents
                .iter()
                .flat_map(|a| a.environment.keys().cloned()),
        );
        keys.extend(
            self.tasks
                .iter()
                .flat_map(|t| t.environment.keys().cloned()),
        );
        if let Some(map) = runner.as_object_mut() {
            if !keys.is_empty() {
                map.insert("allowed_environment_keys".into(), json!(keys));
            }
        }
        runner
    }

    fn analysis_plan(&self, agent: &AgentSpec) -> Option<Value> {
        let comparisons: Vec<&CompareSpec> = self
            .compare
            .iter()
            .filter(|c| c.treatment == agent.agent_id)
            .collect();
        if comparisons.is_empty() {
            return None;
        }
        let question = self.decision_question.clone().unwrap_or_else(|| {
            let baselines: Vec<&str> = comparisons.iter().map(|c| c.baseline.as_str()).collect();
            format!(
                "Does agent {} pass more {} tasks than agent {}?",
                agent.agent_id,
                self.suite_id,
                baselines.join(" or ")
            )
        });
        let declared: Vec<Value> = comparisons
            .iter()
            .map(|c| {
                let mut spec = json!({
                    "comparison_id": c.comparison_id.clone().unwrap_or_else(|| format!("{}-vs-{}", c.treatment, c.baseline)),
                    "baseline_evaluation_id": self.evaluation_id(&c.baseline),
                });
                if let Some(map) = spec.as_object_mut() {
                    if let Some(alpha) = c.alpha {
                        map.insert("alpha".into(), json!(alpha));
                    }
                    if let Some(minimum) = c.minimum_matched_tasks {
                        map.insert("minimum_matched_tasks".into(), json!(minimum));
                    }
                    if let Some(threshold) = c.practical_significance_threshold {
                        map.insert("practical_significance_threshold".into(), json!(threshold));
                    }
                }
                spec
            })
            .collect();
        Some(json!({"decision_question": question, "comparisons": declared}))
    }

    /// One evaluation file per agent, in the order the agents are listed.
    pub fn expand(&self) -> Vec<Expanded> {
        let task_revisions: Vec<Value> = self.tasks.iter().map(|t| self.task_revision(t)).collect();
        let runner = self.runner();
        let retry = self.retry_policy.clone().unwrap_or_else(|| {
            json!({
                "max_attempts_per_trial": 2,
                "retry_retryable_runner_errors": true,
                "retryable_execution_failure_codes": [],
                "retry_timeouts": false,
            })
        });
        self.agents
            .iter()
            .map(|agent| {
                let evaluation_id = self.evaluation_id(&agent.agent_id);
                let fingerprint = agent
                    .fingerprint
                    .clone()
                    .unwrap_or_else(|| format!("FP-{}-{}", self.suite_id, agent.agent_id));
                let mut trials = Vec::new();
                for task in &self.tasks {
                    let mut environment = agent.environment.clone();
                    environment.extend(task.environment.clone());
                    let mut input = json!({
                        "executable_id": agent.executable_id,
                        "args": agent.args,
                        "files": task.files,
                        "environment": environment,
                    });
                    if let Some(map) = input.as_object_mut() {
                        if self.defaults.collect_outputs {
                            map.insert("collect_outputs".into(), json!(true));
                        }
                        if let Some(image) = &agent.image_id {
                            map.insert("image_id".into(), json!(image));
                        }
                        if !agent.credentials.is_empty() {
                            map.insert("credentials".into(), json!(agent.credentials));
                        }
                    }
                    let network = match &agent.network {
                        Some(network) => json!({"mode": "allowlist", "hosts": network.hosts}),
                        None => json!({"mode": "deny_all"}),
                    };
                    for repeat in 1..=self.repeats {
                        trials.push(json!({
                            "trial_ref": {"record_id": self.trial_id(agent, task, repeat), "record_type": "trial"},
                            "task_revision_ref": {"record_id": self.task_revision_id(&task.task_id), "record_type": "task_revision"},
                            "system_fingerprint_ref": {"record_id": fingerprint, "record_type": "system_fingerprint"},
                            "task_input": input,
                            "budget": self.budget(task),
                            "network_policy": network,
                            "side_effect_policy": {"mode": "deny_all"},
                            "capability_grant_refs": [],
                        }));
                    }
                }
                let mut file = json!({
                    "format": FORMAT,
                    "schedule": {
                        "evaluation_ref": {"record_id": evaluation_id, "record_type": "evaluation"},
                        "plan_ref": {"record_id": format!("PLAN-{evaluation_id}"), "record_type": "evaluation_plan"},
                        "max_active_runs": self.defaults.max_active_runs.unwrap_or(2),
                        "retry_policy": retry,
                        "trials": trials,
                    },
                    "runner": runner,
                    "verifier": self.verifier(agent),
                    "project_ref": self.project_ref.clone().unwrap_or_else(|| json!({"record_id": "PROJECT-LOCAL", "record_type": "project"})),
                    "store_output_evidence": self.store_output_evidence,
                    "task_revisions": task_revisions,
                });
                if let Some(map) = file.as_object_mut() {
                    if let Some(plan) = self.analysis_plan(agent) {
                        map.insert("analysis_plan".into(), plan);
                    }
                    if let Some(pricing) = &self.pricing {
                        map.insert("pricing".into(), pricing.clone());
                    }
                    if let Some(readiness) = &self.readiness {
                        map.insert("readiness".into(), readiness.clone());
                    }
                }
                Expanded {
                    evaluation_id,
                    file,
                }
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn suite(extra: Value) -> Suite {
        let mut value = json!({
            "format": SUITE_FORMAT,
            "suite_id": "S",
            "repeats": 3,
            "runner": {"type": "local_bubblewrap", "allowed_executables": {"sh": "/usr/bin/sh"},
                       "limits": {"max_wall_time_ms": 5000, "max_artifact_bytes": 1000},
                       "allowed_environment_keys": ["EXISTING"]},
            "defaults": {"collect_outputs": true},
            "agents": [
                {"agent_id": "A", "executable_id": "sh", "args": ["-c", "a"], "environment": {"MODE": "a"}},
                {"agent_id": "B", "executable_id": "sh", "args": ["-c", "b"]}
            ],
            "tasks": [
                {"task_id": "T1", "objective": "one", "environment": {"PROMPT": "p1"},
                 "checks": [{"criterion_id": "c", "kind": "stdout_contains", "text": "x"}]},
                {"task_id": "T2", "objective": "two", "budget": {"max_wall_time_ms": 1000},
                 "checks": [{"criterion_id": "c", "kind": "file_exists", "path": "out"}],
                 "judge": [{"criterion_id": "j", "rubric": "good"}]}
            ],
            "judge": {"consent_to_send_outputs": true},
            "compare": [{"baseline": "A", "treatment": "B", "minimum_matched_tasks": 5}]
        });
        if let (Some(map), Some(extra)) = (value.as_object_mut(), extra.as_object()) {
            map.extend(extra.clone());
        }
        let suite: Suite = serde_json::from_value(value).unwrap();
        suite.check().unwrap();
        suite
    }

    #[test]
    fn expands_one_evaluation_per_agent_with_shared_tasks() {
        let expanded = suite(json!({})).expand();
        assert_eq!(expanded.len(), 2);
        let (a, b) = (&expanded[0].file, &expanded[1].file);
        assert_eq!(expanded[0].evaluation_id, "S-A");
        let trials = a["schedule"]["trials"].as_array().unwrap();
        assert_eq!(trials.len(), 6);
        assert_eq!(trials[0]["trial_ref"]["record_id"], "TRIAL-S-A-T1-R1");
        assert_eq!(trials[5]["trial_ref"]["record_id"], "TRIAL-S-A-T2-R3");
        assert_eq!(
            trials[0]["task_input"]["environment"],
            json!({"MODE": "a", "PROMPT": "p1"})
        );
        assert_eq!(trials[0]["task_input"]["collect_outputs"], true);
        assert_eq!(trials[0]["budget"]["max_wall_time_ms"], 5000);
        assert_eq!(trials[3]["budget"]["max_wall_time_ms"], 1000);
        assert_eq!(trials[0]["budget"]["max_artifact_bytes"], 1000);
        assert_eq!(
            a["runner"]["allowed_environment_keys"],
            json!(["EXISTING", "MODE", "PROMPT"])
        );
        // Both agents share identical task revisions, so they compare.
        assert_eq!(a["task_revisions"], b["task_revisions"]);
        assert_eq!(
            a["task_revisions"][1]["verification_plan"]["required_criterion_ids"],
            json!(["c", "j"])
        );
        // T2 has a rubric: checks and the judge are combined.
        assert_eq!(a["verifier"]["type"], "composite");
        // The comparison is declared in the treatment's plan only.
        assert!(a.get("analysis_plan").is_none());
        assert_eq!(
            b["analysis_plan"]["comparisons"][0],
            json!({"comparison_id": "B-vs-A", "baseline_evaluation_id": "S-A", "minimum_matched_tasks": 5})
        );
    }

    #[test]
    fn expansion_is_deterministic() {
        let first = suite(json!({})).expand();
        let second = suite(json!({})).expand();
        for (x, y) in first.iter().zip(&second) {
            assert_eq!(x.file, y.file);
        }
    }

    #[test]
    fn network_and_credentials_become_an_allowlist() {
        let expanded = suite(json!({
            "agents": [{"agent_id": "A", "executable_id": "sh", "network": {"hosts": ["api.example.com"]}, "credentials": ["KEY"]}],
            "compare": []
        }))
        .expand();
        let trial = &expanded[0].file["schedule"]["trials"][0];
        assert_eq!(
            trial["network_policy"],
            json!({"mode": "allowlist", "hosts": ["api.example.com"]})
        );
        assert_eq!(trial["task_input"]["credentials"], json!(["KEY"]));
    }
}
