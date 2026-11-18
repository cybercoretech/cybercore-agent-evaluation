use anyhow::{bail, Result};
use cybercore_evaluation_core::{
    execute_with_runner, ExecutionBudget, ExecutionDisposition, ExecutionRequest,
    LocalProcessRunner, LocalProcessRunnerConfig, NetworkPolicy, RecordReference, SideEffectPolicy,
};
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

fn reference(record_type: &str, record_id: &str) -> RecordReference {
    RecordReference {
        record_id: record_id.to_owned(),
        record_type: record_type.to_owned(),
        schema_version: Some("1.0.0".to_owned()),
    }
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<()> {
    let bubblewrap_path = std::env::var_os("BWRAP")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/usr/bin/bwrap"));

    let config = LocalProcessRunnerConfig {
        runner_id: "local-bwrap-smoke".to_owned(),
        runner_version: env!("CARGO_PKG_VERSION").to_owned(),
        bubblewrap_path,
        temporary_root: std::env::temp_dir().join("cybercore-local-runner-inputs"),
        allowed_executables: BTreeMap::from([("cat".to_owned(), PathBuf::from("/usr/bin/cat"))]),
        allowed_environment_keys: BTreeSet::new(),
        max_wall_time_ms: 10_000,
        max_output_bytes: 4_096,
        max_workspace_bytes: 16 * 1024 * 1024,
        max_tmp_bytes: 8 * 1024 * 1024,
        max_input_file_bytes: 1024 * 1024,
        max_input_files: 8,
        max_arguments: 64,
        max_argument_bytes: 64 * 1024,
    };

    let runner = LocalProcessRunner::new(config)?;
    let request = ExecutionRequest {
        request_id: "REQ-LOCAL-SMOKE-001".to_owned(),
        idempotency_key: "IDEM-LOCAL-SMOKE-001".to_owned(),
        run_ref: reference("run", "RUN-LOCAL-SMOKE-001"),
        evaluation_ref: reference("evaluation", "EVAL-LOCAL-SMOKE-001"),
        trial_ref: reference("trial", "TRIAL-LOCAL-SMOKE-001"),
        task_revision_ref: reference("task_revision", "TASK-LOCAL-SMOKE-001"),
        system_fingerprint_ref: reference("system_fingerprint", "SYSTEM-LOCAL-SMOKE-001"),
        task_input: json!({
            "executable_id": "cat",
            "args": ["/workspace/input/smoke.txt"],
            "files": [{
                "path": "smoke.txt",
                "content": "local-runner-smoke\n"
            }],
            "environment": {}
        }),
        budget: ExecutionBudget {
            max_wall_time_ms: 10_000,
            max_output_bytes: 4_096,
            max_artifact_bytes: 0,
            max_tool_calls: 0,
            max_input_tokens: None,
            max_output_tokens: None,
        },
        network_policy: NetworkPolicy::DenyAll,
        side_effect_policy: SideEffectPolicy::DenyAll,
        capability_grant_refs: Vec::new(),
    };

    let result = execute_with_runner(&runner, &request).await?;
    let stdout = result
        .output
        .as_ref()
        .and_then(|output| output.get("stdout"))
        .and_then(Value::as_str)
        .unwrap_or_default();

    if result.disposition != ExecutionDisposition::Finished || stdout != "local-runner-smoke\n" {
        bail!(
            "local Bubblewrap smoke test failed: disposition={:?}, stdout={stdout:?}, output={:?}",
            result.disposition,
            result.output
        );
    }

    println!("PASS: Bubblewrap local process runner executed cat in its isolated workspace.");
    println!("{}", serde_json::to_string_pretty(&result)?);
    Ok(())
}
