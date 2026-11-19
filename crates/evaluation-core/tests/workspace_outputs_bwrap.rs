//! Real-sandbox tests for `/workspace/output` collection. They skip (loudly)
//! where Bubblewrap cannot create namespaces, unless WORKBENCH_REQUIRE_BWRAP
//! is set, in which case an unusable sandbox fails the test.

use cybercore_evaluation_core::{
    artifact_staging_dir, execute_with_runner, ExecutionBudget, ExecutionDisposition,
    ExecutionRequest, LocalProcessRunner, LocalProcessRunnerConfig, NetworkPolicy, RecordReference,
    RunnerErrorKind, SideEffectPolicy,
};
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

fn reference(record_type: &str, record_id: &str) -> RecordReference {
    RecordReference {
        record_id: record_id.into(),
        record_type: record_type.into(),
        schema_version: None,
    }
}

fn temp(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "cybercore-bwrap-outputs-{}-{name}",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    dir
}

/// A runner allowed to run `sh`, or `None` when the sandbox is unavailable.
fn runner(root: &Path, artifacts: bool) -> Option<LocalProcessRunner> {
    let config = LocalProcessRunnerConfig {
        runner_id: "outputs-test".into(),
        runner_version: "0".into(),
        bubblewrap_path: PathBuf::from("/usr/bin/bwrap"),
        temporary_root: root.join("inputs"),
        allowed_executables: BTreeMap::from([("sh".into(), PathBuf::from("/usr/bin/sh"))]),
        allowed_environment_keys: BTreeSet::new(),
        max_wall_time_ms: 10_000,
        max_output_bytes: 4096,
        max_workspace_bytes: 4 * 1024 * 1024,
        max_tmp_bytes: 1024 * 1024,
        max_input_file_bytes: 64 * 1024,
        max_input_files: 8,
        max_arguments: 16,
        max_argument_bytes: 16 * 1024,
        artifact_root: artifacts.then(|| root.join("artifacts")),
        max_artifact_bytes: 64 * 1024,
        max_output_files: 16,
    };
    match LocalProcessRunner::new(config) {
        Ok(runner) => Some(runner),
        Err(error) if error.kind == RunnerErrorKind::Unavailable => {
            assert!(
                std::env::var_os("WORKBENCH_REQUIRE_BWRAP").is_none(),
                "WORKBENCH_REQUIRE_BWRAP is set but Bubblewrap is unavailable: {}",
                error.message
            );
            eprintln!("SKIPPED: Bubblewrap is unavailable here: {}", error.message);
            None
        }
        Err(error) => panic!("runner config rejected: {error:?}"),
    }
}

fn request(id: &str, script: &str, max_artifact_bytes: u64, wall_ms: u64) -> ExecutionRequest {
    ExecutionRequest {
        request_id: id.into(),
        idempotency_key: format!("IDEMP-{id}"),
        run_ref: reference("run", &format!("RUN-{id}")),
        evaluation_ref: reference("evaluation", "EVAL-1"),
        trial_ref: reference("trial", "TRIAL-1"),
        task_revision_ref: reference("task_revision", "TASK-1"),
        system_fingerprint_ref: reference("system_fingerprint", "FP-1"),
        task_input: json!({
            "executable_id": "sh",
            "args": ["-c", script],
            "collect_outputs": true
        }),
        budget: ExecutionBudget {
            max_wall_time_ms: wall_ms,
            max_output_bytes: 4096,
            max_artifact_bytes,
            max_tool_calls: 0,
            max_input_tokens: None,
            max_output_tokens: None,
        },
        network_policy: NetworkPolicy::DenyAll,
        side_effect_policy: SideEffectPolicy::DenyAll,
        capability_grant_refs: vec![],
    }
}

fn export(result: &cybercore_evaluation_core::RunnerResult) -> &str {
    result.output.as_ref().unwrap()["workspace_file_artifact_export"]
        .as_str()
        .unwrap()
}

#[tokio::test]
async fn regular_files_are_collected_and_links_are_refused() {
    let root = temp("collect");
    let Some(runner) = runner(&root, true) else {
        return;
    };
    let script = "mkdir -p /workspace/output/sub \
        && printf 'report body' > /workspace/output/report.txt \
        && printf abc > /workspace/output/sub/a.txt \
        && ln -s /etc/passwd /workspace/output/leak \
        && mkfifo /workspace/output/pipe";
    let req = request("REQ-COLLECT", script, 4096, 10_000);
    let result = execute_with_runner(&runner, &req).await.unwrap();
    assert_eq!(
        result.disposition,
        ExecutionDisposition::Finished,
        "{:?}",
        result.output
    );
    assert_eq!(export(&result), "collected");

    let ids: Vec<&str> = result
        .artifacts
        .iter()
        .map(|a| a.artifact_id.as_str())
        .collect();
    assert_eq!(ids, vec!["report.txt", "sub/a.txt"]);
    let abc = result
        .artifacts
        .iter()
        .find(|a| a.artifact_id == "sub/a.txt")
        .unwrap();
    assert_eq!(
        abc.sha256_hex,
        "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
    );
    assert_eq!(result.resource_usage.artifact_bytes, 14);

    let rejected: Vec<&str> = result.output.as_ref().unwrap()["rejected_output_entries"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e: &Value| e["path"].as_str().unwrap())
        .collect();
    assert_eq!(rejected, vec!["leak", "pipe"]);

    let staged = artifact_staging_dir(
        &fs::canonicalize(root.join("artifacts")).unwrap(),
        "REQ-COLLECT",
    );
    assert_eq!(fs::read(staged.join("report.txt")).unwrap(), b"report body");
    assert!(fs::symlink_metadata(staged.join("leak"))
        .unwrap()
        .file_type()
        .is_symlink());
    fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn outputs_over_the_artifact_budget_are_discarded() {
    let root = temp("limit");
    let Some(runner) = runner(&root, true) else {
        return;
    };
    let script = "head -c 2000 /dev/zero > /workspace/output/big.bin";
    let req = request("REQ-LIMIT", script, 1000, 10_000);
    let result = execute_with_runner(&runner, &req).await.unwrap();
    assert_eq!(result.disposition, ExecutionDisposition::Errored);
    assert_eq!(
        result.failure.as_ref().unwrap().code,
        "artifact_limit_exceeded"
    );
    assert_eq!(export(&result), "discarded_over_limit");
    assert!(result.artifacts.is_empty());
    let staged = artifact_staging_dir(
        &fs::canonicalize(root.join("artifacts")).unwrap(),
        "REQ-LIMIT",
    );
    assert!(!staged.exists());
    fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn outputs_of_killed_runs_are_never_collected() {
    let root = temp("timeout");
    let Some(runner) = runner(&root, true) else {
        return;
    };
    let script = "printf partial > /workspace/output/a.txt; sleep 5";
    let req = request("REQ-TIMEOUT", script, 4096, 300);
    let result = execute_with_runner(&runner, &req).await.unwrap();
    assert_eq!(result.disposition, ExecutionDisposition::TimedOut);
    assert_eq!(export(&result), "skipped_after_termination");
    assert!(result.artifacts.is_empty());
    let artifacts = fs::canonicalize(root.join("artifacts")).unwrap();
    assert_eq!(
        fs::read_dir(&artifacts).unwrap().count(),
        0,
        "nothing staged"
    );
    fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn collection_needs_an_artifact_root_and_budget() {
    let root = temp("refused");
    let Some(without_root) = runner(&root, false) else {
        return;
    };
    let error = execute_with_runner(&without_root, &request("REQ-NOROOT", "true", 4096, 10_000))
        .await
        .unwrap_err();
    assert_eq!(error.kind, RunnerErrorKind::InvalidRequest);

    let with_root = runner(&root, true).unwrap();
    let error = execute_with_runner(&with_root, &request("REQ-NOBUDGET", "true", 0, 10_000))
        .await
        .unwrap_err();
    assert_eq!(error.kind, RunnerErrorKind::InvalidRequest);
    fs::remove_dir_all(root).unwrap();
}
