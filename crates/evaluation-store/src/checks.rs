//! A deterministic verifier with several criteria per trial, over the
//! runner's stdout and the files the task wrote to `/workspace/output`.

use crate::recorder::{
    CriterionVerdict, VerdictResult, VerificationInput, Verifier, VerifierIdentity, VerifierType,
};
use serde_json::Value;
use std::collections::BTreeMap;

/// What one criterion checks.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Check {
    /// Captured stdout equals `expected` exactly.
    StdoutEquals { expected: String },
    /// Captured stdout contains `text`.
    StdoutContains { text: String },
    /// `path` was left in `/workspace/output` as a regular file.
    FileExists { path: String },
    /// Nothing was left at `path` (no file, link or other entry).
    FileAbsent { path: String },
    /// The file at `path` equals `expected` byte for byte.
    FileEquals { path: String, expected: String },
    /// The file at `path` contains `text`.
    FileContains { path: String, text: String },
}

/// A check tied to the acceptance criterion it judges.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CriterionCheck {
    /// Matches a `task_revision` acceptance criterion so ADR-0013's outcome
    /// rules apply; `[A-Za-z0-9][A-Za-z0-9._:-]*`.
    pub criterion_id: String,
    pub check: Check,
}

/// Runs each trial's checks. Trials without checks are reported as
/// unavailable, never as passing.
pub struct ChecksVerifier {
    checks: BTreeMap<String, Vec<CriterionCheck>>,
}

impl ChecksVerifier {
    /// `checks` maps trial IDs to their criteria.
    pub fn new(checks: BTreeMap<String, Vec<CriterionCheck>>) -> Self {
        Self { checks }
    }
}

impl Verifier for ChecksVerifier {
    fn identity(&self) -> VerifierIdentity {
        VerifierIdentity {
            verifier_id: "workbench-checks".into(),
            version: env!("CARGO_PKG_VERSION").into(),
            verifier_type: VerifierType::Deterministic,
        }
    }

    fn verify(&self, input: &VerificationInput<'_>) -> Vec<CriterionVerdict> {
        let trial_id = &input.trial.trial_ref.record_id;
        let Some(checks) = self.checks.get(trial_id).filter(|c| !c.is_empty()) else {
            return vec![CriterionVerdict {
                criterion_id: "no-checks-configured".into(),
                inspected_target: format!("trial:{trial_id}"),
                method: "Look up the checks configured for this trial.".into(),
                result: VerdictResult::Unavailable,
                limitations: vec![format!("No checks are configured for {trial_id}.")],
                judge_rationale: None,
                evidence_paths: Vec::new(),
            }];
        };
        checks.iter().map(|c| judge(c, input)).collect()
    }
}

fn judge(criterion: &CriterionCheck, input: &VerificationInput<'_>) -> CriterionVerdict {
    let output = input.result.output.as_ref();
    let verdict = |target: String,
                   method: &str,
                   result,
                   limitations: Vec<String>,
                   rationale: Option<String>,
                   paths: Vec<String>| {
        CriterionVerdict {
            criterion_id: criterion.criterion_id.clone(),
            inspected_target: target,
            method: method.into(),
            result,
            limitations,
            judge_rationale: rationale,
            evidence_paths: paths,
        }
    };
    let pass_fail = |ok: bool| {
        if ok {
            VerdictResult::Pass
        } else {
            VerdictResult::Fail
        }
    };

    match &criterion.check {
        Check::StdoutEquals { .. } | Check::StdoutContains { .. } => {
            let target = "runner-output:stdout".to_string();
            let stdout = output.and_then(|o| o.get("stdout")).and_then(Value::as_str);
            let truncated = output
                .and_then(|o| o.get("output_truncated"))
                .and_then(Value::as_bool)
                .unwrap_or(false);
            let (method, ok) = match (&criterion.check, stdout) {
                (_, None) => {
                    return verdict(
                        target,
                        "Inspect captured stdout.",
                        VerdictResult::Unavailable,
                        vec!["The runner result has no captured stdout.".into()],
                        None,
                        vec![],
                    );
                }
                (Check::StdoutEquals { expected }, Some(s)) => (
                    "Compare captured stdout byte-for-byte with the expected text.",
                    s == expected,
                ),
                (Check::StdoutContains { text }, Some(s)) => (
                    "Search captured stdout for the expected text.",
                    s.contains(text.as_str()),
                ),
                // Not reachable (the outer match selected stdout checks); a
                // verdict instead of a panic keeps the verifier total.
                _ => return mismatch(criterion),
            };
            // A truncated stdout can still prove containment, nothing else.
            if truncated && !(ok && matches!(criterion.check, Check::StdoutContains { .. })) {
                return verdict(
                    target,
                    method,
                    VerdictResult::Inconclusive,
                    vec!["Captured stdout was truncated by the runner's output limit.".into()],
                    None,
                    vec![],
                );
            }
            verdict(target, method, pass_fail(ok), vec![], None, vec![])
        }
        Check::FileExists { path }
        | Check::FileAbsent { path }
        | Check::FileEquals { path, .. }
        | Check::FileContains { path, .. } => {
            let target = format!("workspace-output:{path}");
            let export = output
                .and_then(|o| o.get("workspace_file_artifact_export"))
                .and_then(Value::as_str)
                .unwrap_or("unknown");
            if export != "collected" {
                return verdict(
                    target,
                    "Inspect the collected workspace outputs.",
                    VerdictResult::Unavailable,
                    vec![format!(
                        "Workspace outputs were not collected for this run (export: {export})."
                    )],
                    None,
                    vec![],
                );
            }
            let listed = input
                .result
                .artifacts
                .iter()
                .any(|a| &a.artifact_id == path);
            let rejected = output
                .and_then(|o| o.get("rejected_output_entries"))
                .and_then(Value::as_array)
                .and_then(|all| {
                    all.iter()
                        .find(|e| e["path"].as_str() == Some(path.as_str()))
                })
                .and_then(|e| e["reason"].as_str());
            let content = input.files.get(path);
            if listed && content.is_none() {
                return verdict(target, "Read the collected file.", VerdictResult::Unavailable,
                    vec![format!("{path} was collected but its staged copy is missing or does not match its digest.")],
                    None, vec![]);
            }
            match &criterion.check {
                Check::FileExists { .. } => {
                    let rationale =
                        rejected.map(|r| format!("{path} exists but was not collected: {r}."));
                    verdict(
                        target,
                        "Check that the file was left as a regular file.",
                        pass_fail(content.is_some()),
                        vec![],
                        rationale,
                        vec![path.clone()],
                    )
                }
                Check::FileAbsent { .. } => {
                    let rationale = rejected.map(|r| format!("An entry exists at {path} ({r})."));
                    verdict(
                        target,
                        "Check that nothing was left at the path.",
                        pass_fail(content.is_none() && rejected.is_none()),
                        vec![],
                        rationale,
                        vec![],
                    )
                }
                Check::FileEquals { expected, .. } => {
                    let rationale = match (content, rejected) {
                        (None, Some(r)) => Some(format!("{path} was not collected: {r}.")),
                        (None, None) => Some(format!("{path} was not produced.")),
                        _ => None,
                    };
                    let ok = content.is_some_and(|bytes| bytes == expected.as_bytes());
                    verdict(
                        target,
                        "Compare the file byte-for-byte with the expected text.",
                        pass_fail(ok),
                        vec![],
                        rationale,
                        vec![path.clone()],
                    )
                }
                Check::FileContains { text, .. } => {
                    let rationale = content
                        .is_none()
                        .then(|| format!("{path} was not produced."));
                    let ok = content.is_some_and(|bytes| {
                        let needle = text.as_bytes();
                        needle.is_empty() || bytes.windows(needle.len()).any(|w| w == needle)
                    });
                    verdict(
                        target,
                        "Search the file for the expected text.",
                        pass_fail(ok),
                        vec![],
                        rationale,
                        vec![path.clone()],
                    )
                }
                _ => mismatch(criterion),
            }
        }
    }
}

fn mismatch(criterion: &CriterionCheck) -> CriterionVerdict {
    CriterionVerdict {
        criterion_id: criterion.criterion_id.clone(),
        inspected_target: "verifier".into(),
        method: "Dispatch the check.".into(),
        result: VerdictResult::Unavailable,
        limitations: vec!["Internal verifier dispatch error; the check was not run.".into()],
        judge_rationale: None,
        evidence_paths: Vec::new(),
    }
}
