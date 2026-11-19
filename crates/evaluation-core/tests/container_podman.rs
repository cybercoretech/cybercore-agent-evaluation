//! Real rootless-Podman tests for `ContainerRunner`. They need podman and the
//! local image `docker.io/library/ubuntu:22.04`; they skip (loudly) otherwise,
//! unless WORKBENCH_REQUIRE_PODMAN is set, in which case they fail.

use cybercore_evaluation_core::{
    artifact_staging_dir, container_name, execute_with_runner, ContainerRunner,
    ContainerRunnerConfig, ExecutionBudget, ExecutionDisposition, ExecutionRequest, NetworkPolicy,
    RecordReference, RunnerErrorKind, RunnerResult, SideEffectPolicy,
};
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

const IMAGE: &str = "docker.io/library/ubuntu:22.04";

fn reference(record_type: &str, record_id: &str) -> RecordReference {
    RecordReference {
        record_id: record_id.into(),
        record_type: record_type.into(),
        schema_version: None,
    }
}

fn temp(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("cybercore-podman-{}-{name}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    dir
}

fn runner(root: &Path) -> Option<ContainerRunner> {
    let config = ContainerRunnerConfig {
        runner_id: "podman-test".into(),
        runner_version: "0".into(),
        podman_path: PathBuf::from("/usr/bin/podman"),
        temporary_root: root.join("inputs"),
        images: BTreeMap::from([("ubuntu".into(), IMAGE.into())]),
        allowed_executables: BTreeMap::from([("sh".into(), "/bin/sh".into())]),
        allowed_environment_keys: BTreeSet::from(["GREETING".to_string()]),
        max_wall_time_ms: 30_000,
        max_output_bytes: 8192,
        memory_bytes: 64 * 1024 * 1024,
        cpu_millis: 500,
        pids_limit: 32,
        workspace_bytes: 8 * 1024 * 1024,
        tmp_bytes: 4 * 1024 * 1024,
        max_input_file_bytes: 64 * 1024,
        max_input_files: 8,
        max_arguments: 16,
        max_argument_bytes: 16 * 1024,
        artifact_root: Some(root.join("artifacts")),
        max_artifact_bytes: 1024 * 1024,
        max_output_files: 16,
    };
    match ContainerRunner::new(config) {
        Ok(runner) => Some(runner),
        Err(error) => {
            assert!(
                std::env::var_os("WORKBENCH_REQUIRE_PODMAN").is_none(),
                "WORKBENCH_REQUIRE_PODMAN is set but the container runner is unavailable: {}",
                error.message
            );
            eprintln!(
                "SKIPPED: container runner unavailable here: {}",
                error.message
            );
            None
        }
    }
}

fn request(
    id: &str,
    script: &str,
    task_extra: Value,
    artifact_bytes: u64,
    wall_ms: u64,
) -> ExecutionRequest {
    let mut task_input = json!({"executable_id": "sh", "args": ["-c", script]});
    if let (Some(map), Some(extra)) = (task_input.as_object_mut(), task_extra.as_object()) {
        map.extend(extra.clone());
    }
    ExecutionRequest {
        request_id: id.into(),
        idempotency_key: format!("IDEMP-{id}"),
        run_ref: reference("run", &format!("RUN-{id}")),
        evaluation_ref: reference("evaluation", "EVAL-1"),
        trial_ref: reference("trial", "TRIAL-1"),
        task_revision_ref: reference("task_revision", "TASK-1"),
        system_fingerprint_ref: reference("system_fingerprint", "FP-1"),
        task_input,
        budget: ExecutionBudget {
            max_wall_time_ms: wall_ms,
            max_output_bytes: 8192,
            max_artifact_bytes: artifact_bytes,
            max_tool_calls: 0,
            max_input_tokens: None,
            max_output_tokens: None,
        },
        network_policy: NetworkPolicy::DenyAll,
        side_effect_policy: SideEffectPolicy::DenyAll,
        capability_grant_refs: vec![],
    }
}

fn out(result: &RunnerResult, key: &str) -> Value {
    result.output.as_ref().unwrap()[key].clone()
}

/// The request's container must be gone (stopped and removed) when the call
/// returns.
fn assert_no_leftover_container(request_id: &str) {
    let label = format!("label=io.cybercore.request={}", container_name(request_id));
    let listing = Command::new("podman")
        .args(["ps", "-a", "--filter", &label, "--format", "{{.Names}}"])
        .output()
        .unwrap();
    let names = String::from_utf8_lossy(&listing.stdout);
    assert!(names.trim().is_empty(), "leftover containers: {names}");
}

#[tokio::test]
async fn runs_in_a_locked_down_container_and_cleans_up() {
    let root = temp("basic");
    let Some(runner) = runner(&root) else { return };
    let script = "printf '%s|' \"$GREETING\" \"$(id -u)\" \"$(ls /sys/class/net)\"; \
        cat /workspace/input/in.txt; touch /etc/x 2>/dev/null || printf '|ro'";
    let req = request(
        "REQ-BASIC",
        script,
        json!({"files": [{"path": "in.txt", "content": "input!"}], "environment": {"GREETING": "hi"}}),
        0,
        30_000,
    );
    let result = execute_with_runner(&runner, &req).await.unwrap();
    assert_eq!(
        result.disposition,
        ExecutionDisposition::Finished,
        "{:?}",
        result.output
    );
    let stdout = out(&result, "stdout");
    let uid = String::from_utf8_lossy(&Command::new("id").arg("-u").output().unwrap().stdout)
        .trim()
        .to_string();
    // Allowlisted env, keep-id user, loopback-only network, read-only inputs, read-only root.
    assert_eq!(stdout.as_str().unwrap(), format!("hi|{uid}|lo|input!|ro"));
    assert!(out(&result, "container")["image_digest"]
        .as_str()
        .unwrap()
        .starts_with("sha256:"));
    assert_eq!(
        out(&result, "workspace_file_artifact_export"),
        "not_requested"
    );
    assert_no_leftover_container("REQ-BASIC");
    fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn memory_limit_kills_and_is_reported() {
    let root = temp("memory");
    let Some(runner) = runner(&root) else { return };
    let result = execute_with_runner(
        &runner,
        &request("REQ-OOM", "tail /dev/zero", json!({}), 0, 30_000),
    )
    .await
    .unwrap();
    assert_eq!(result.disposition, ExecutionDisposition::Errored);
    assert_eq!(
        result.failure.as_ref().unwrap().code,
        "memory_limit_exceeded"
    );
    assert_eq!(out(&result, "container")["oom_killed"], true);
    assert_no_leftover_container("REQ-OOM");
    fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn process_limit_stops_fork_bombs() {
    let root = temp("pids");
    let Some(runner) = runner(&root) else { return };
    let script = "for i in $(seq 1 200); do sleep 3 & done; echo spawned";
    let result = execute_with_runner(&runner, &request("REQ-PIDS", script, json!({}), 0, 30_000))
        .await
        .unwrap();
    assert!(
        out(&result, "stderr").as_str().unwrap().contains("fork"),
        "{:?}",
        result.output
    );
    assert_no_leftover_container("REQ-PIDS");
    fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn outputs_are_collected_from_a_frozen_container() {
    let root = temp("outputs");
    let Some(runner) = runner(&root) else { return };
    // A background writer keeps running after the main process exits; the
    // pause freezes it, so collection sees a stable tree.
    let script =
        "umask 077; mkdir -p /workspace/output/sub; printf private > /workspace/output/sub/p.txt; \
        ln -s /etc/passwd /workspace/output/leak; \
        (while true; do date >> /workspace/output/log.txt; sleep 0.1; done) & sleep 0.3; echo done";
    let req = request(
        "REQ-OUT",
        script,
        json!({"collect_outputs": true}),
        64 * 1024,
        30_000,
    );
    let result = execute_with_runner(&runner, &req).await.unwrap();
    assert_eq!(
        result.disposition,
        ExecutionDisposition::Finished,
        "{:?}",
        result.output
    );
    assert_eq!(out(&result, "workspace_file_artifact_export"), "collected");
    let ids: Vec<&str> = result
        .artifacts
        .iter()
        .map(|a| a.artifact_id.as_str())
        .collect();
    assert_eq!(ids, vec!["log.txt", "sub/p.txt"]);
    let rejected = out(&result, "rejected_output_entries");
    assert_eq!(rejected[0]["path"], "leak");
    let staged = artifact_staging_dir(
        &fs::canonicalize(root.join("artifacts")).unwrap(),
        "REQ-OUT",
    );
    // A 0600 file the agent wrote is readable thanks to --userns keep-id.
    assert_eq!(fs::read(staged.join("sub/p.txt")).unwrap(), b"private");
    assert!(!staged.join("leak").exists(), "links are never copied");
    assert_no_leftover_container("REQ-OUT");
    fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn output_tmpfs_is_capped_at_the_artifact_budget() {
    let root = temp("cap");
    let Some(runner) = runner(&root) else { return };
    let script = "head -c 200000 /dev/zero > /workspace/output/big.bin || echo capped";
    let req = request(
        "REQ-CAP",
        script,
        json!({"collect_outputs": true}),
        64 * 1024,
        30_000,
    );
    let result = execute_with_runner(&runner, &req).await.unwrap();
    assert!(
        out(&result, "stdout").as_str().unwrap().contains("capped"),
        "{:?}",
        result.output
    );
    assert!(result.resource_usage.artifact_bytes <= 64 * 1024);
    assert_no_leftover_container("REQ-CAP");
    fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn timeouts_stop_the_container_and_skip_outputs() {
    let root = temp("timeout");
    let Some(runner) = runner(&root) else { return };
    let req = request(
        "REQ-SLOW",
        "printf x > /workspace/output/a; sleep 30",
        json!({"collect_outputs": true}),
        4096,
        1_000,
    );
    let result = execute_with_runner(&runner, &req).await.unwrap();
    assert_eq!(result.disposition, ExecutionDisposition::TimedOut);
    assert_eq!(
        out(&result, "workspace_file_artifact_export"),
        "skipped_after_termination"
    );
    assert!(result.artifacts.is_empty());
    assert_no_leftover_container("REQ-SLOW");
    fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn unknown_images_and_missing_artifact_budgets_are_refused() {
    let root = temp("refused");
    let Some(runner) = runner(&root) else { return };
    let error = execute_with_runner(
        &runner,
        &request("REQ-IMG", "true", json!({"image_id": "nope"}), 0, 30_000),
    )
    .await
    .unwrap_err();
    assert_eq!(error.kind, RunnerErrorKind::InvalidRequest);
    let error = execute_with_runner(
        &runner,
        &request(
            "REQ-NOBUDGET",
            "true",
            json!({"collect_outputs": true}),
            0,
            30_000,
        ),
    )
    .await
    .unwrap_err();
    assert_eq!(error.kind, RunnerErrorKind::InvalidRequest);
    fs::remove_dir_all(root).unwrap();
}
