//! Real rootless-Podman tests for allowlisted egress (ADR-0019). They build a
//! statically linked forwarder, need podman and the local image
//! `docker.io/library/ubuntu:22.04`, and skip (loudly) otherwise unless
//! WORKBENCH_REQUIRE_PODMAN is set. No traffic leaves the machine: the only
//! allowlisted destination is a TLS stub on the host's loopback.

use cybercore_evaluation_core::{
    artifact_staging_dir, execute_with_runner, ContainerEgressConfig, ContainerRunner,
    ContainerRunnerConfig, ExecutionBudget, ExecutionDisposition, ExecutionRequest, NetworkPolicy,
    RecordReference, RunnerErrorKind, SideEffectPolicy,
};
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;

const IMAGE: &str = "docker.io/library/ubuntu:22.04";
const TOKEN: &str = "sk-test-0123456789abcdef";

fn reference(record_type: &str, record_id: &str) -> RecordReference {
    RecordReference {
        record_id: record_id.into(),
        record_type: record_type.into(),
        schema_version: None,
    }
}

fn temp(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("cybercore-egress-{}-{name}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    dir
}

/// Builds the forwarder statically, once per test binary.
fn static_forwarder() -> PathBuf {
    static PATH: OnceLock<PathBuf> = OnceLock::new();
    PATH.get_or_init(|| {
        let target_dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("static-forwarder");
        let status = Command::new(std::env::var("CARGO").unwrap_or_else(|_| "cargo".into()))
            .args([
                "build",
                "--quiet",
                "--release",
                "--locked",
                "-p",
                "cybercore-egress-forwarder",
                "--target",
                "x86_64-unknown-linux-gnu",
            ])
            .env("CARGO_TARGET_DIR", &target_dir)
            .env_remove("CARGO_ENCODED_RUSTFLAGS")
            .env("RUSTFLAGS", "-C target-feature=+crt-static")
            .current_dir(env!("CARGO_MANIFEST_DIR"))
            .status()
            .expect("cargo runs");
        assert!(status.success(), "building the static forwarder failed");
        target_dir.join("x86_64-unknown-linux-gnu/release/cybercore-egress-forwarder")
    })
    .clone()
}

fn base_runner(root: &Path) -> Option<ContainerRunner> {
    let config = ContainerRunnerConfig {
        runner_id: "podman-egress-test".into(),
        runner_version: "0".into(),
        podman_path: PathBuf::from("/usr/bin/podman"),
        temporary_root: root.join("inputs"),
        images: BTreeMap::from([("ubuntu".into(), IMAGE.into())]),
        allowed_executables: BTreeMap::from([("bash".into(), "/bin/bash".into())]),
        allowed_environment_keys: BTreeSet::from(["PORT".to_string(), "HELLO".to_string()]),
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

fn egress(forwarder: PathBuf) -> ContainerEgressConfig {
    ContainerEgressConfig {
        forwarder_path: forwarder,
        ports: BTreeSet::from([443]),
        max_connections: 8,
        max_bytes: 1024 * 1024,
        allow_private_destinations: true,
        credentials: BTreeMap::from([("TEST_TOKEN".to_string(), TOKEN.to_string())]),
    }
}

fn request(id: &str, script: &str, extra: Value, network: NetworkPolicy) -> ExecutionRequest {
    let mut task_input = json!({"executable_id": "bash", "args": ["-c", script]});
    if let (Some(map), Some(extra)) = (task_input.as_object_mut(), extra.as_object()) {
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
            max_wall_time_ms: 20_000,
            max_output_bytes: 8192,
            max_artifact_bytes: 64 * 1024,
            max_tool_calls: 0,
            max_input_tokens: None,
            max_output_tokens: None,
        },
        network_policy: network,
        side_effect_policy: SideEffectPolicy::DenyAll,
        capability_grant_refs: vec![],
    }
}

/// A TLS stand-in on the host loopback: reads one TLS record, answers PONG.
fn tls_stub() -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || {
        for mut stream in listener.incoming().flatten() {
            let mut header = [0_u8; 5];
            if stream.read_exact(&mut header).is_ok() {
                let mut body = vec![0_u8; usize::from(u16::from_be_bytes([header[3], header[4]]))];
                if stream.read_exact(&mut body).is_ok() {
                    let _ = stream.write_all(b"PONG\n");
                }
            }
        }
    });
    port
}

/// A minimal ClientHello carrying `sni`, as a bash printf escape string.
fn hello_escapes(sni: &str) -> String {
    let name = sni.as_bytes();
    let mut ext = vec![0, 0];
    ext.extend(((name.len() + 5) as u16).to_be_bytes());
    ext.extend(((name.len() + 3) as u16).to_be_bytes());
    ext.push(0);
    ext.extend((name.len() as u16).to_be_bytes());
    ext.extend(name);
    let mut body = vec![3, 3];
    body.extend([7_u8; 32]);
    body.extend([0, 0, 2, 0x13, 1, 1, 0]);
    body.extend((ext.len() as u16).to_be_bytes());
    body.extend(ext);
    let mut handshake = vec![1];
    handshake.extend(&(body.len() as u32).to_be_bytes()[1..]);
    handshake.extend(body);
    let mut record = vec![0x16, 3, 1];
    record.extend((handshake.len() as u16).to_be_bytes());
    record.extend(handshake);
    record.iter().map(|b| format!("\\x{b:02x}")).collect()
}

const SCRIPT: &str = r#"
exec 3<>/dev/tcp/127.0.0.1/3128
printf 'CONNECT localhost:443 HTTP/1.1\r\n\r\n' >&3
IFS= read -r line <&3; echo "allowed-status:${line%$'\r'}"
IFS= read -r line <&3
printf "$HELLO" >&3
IFS= read -r line <&3; echo "reply:$line"
exec 3>&-
exec 4<>/dev/tcp/127.0.0.1/3128
printf 'CONNECT example.com:443 HTTP/1.1\r\n\r\n' >&4
IFS= read -r line <&4; echo "denied-status:${line%$'\r'}"
exec 4>&-
if (exec 5<>/dev/tcp/1.1.1.1/443) 2>/dev/null; then echo direct:open; else echo direct:blocked; fi
echo "proxy-env:$HTTPS_PROXY"
echo "token:$TEST_TOKEN"
echo "$TEST_TOKEN" > /workspace/output/leak.txt
echo fine > /workspace/output/ok.txt
"#;

#[tokio::test]
async fn allowlisted_hosts_only_with_credentials_redacted() {
    let root = temp("allow");
    let Some(runner) = base_runner(&root) else {
        return;
    };
    // The stub listens on an ephemeral loopback port; allow that port too.
    let port = tls_stub();
    let script = SCRIPT.replace("localhost:443", &format!("localhost:{port}"));
    let mut config = egress(static_forwarder());
    config.ports = BTreeSet::from([443, port]);
    let runner = runner.with_egress(config).unwrap();
    let result = execute_with_runner(
        &runner,
        &request(
            "EGRESS-ALLOW",
            &script,
            json!({"environment": {"HELLO": hello_escapes("localhost")}, "credentials": ["TEST_TOKEN"], "collect_outputs": true}),
            NetworkPolicy::Allowlist { hosts: vec!["localhost".into()] },
        ),
    )
    .await
    .unwrap();
    let output = result.output.clone().unwrap();
    let stdout = output["stdout"].as_str().unwrap();
    assert_eq!(
        result.disposition,
        ExecutionDisposition::Finished,
        "{output:#}"
    );
    assert!(
        stdout.contains("allowed-status:HTTP/1.1 200 Connection Established"),
        "{stdout}"
    );
    assert!(stdout.contains("reply:PONG"), "{stdout}");
    assert!(
        stdout.contains("denied-status:HTTP/1.1 403 Forbidden"),
        "{stdout}"
    );
    assert!(stdout.contains("direct:blocked"), "{stdout}");
    assert!(
        stdout.contains("proxy-env:http://127.0.0.1:3128"),
        "{stdout}"
    );
    assert!(stdout.contains("token:[REDACTED:TEST_TOKEN]"), "{stdout}");
    assert!(
        !serde_json::to_string(&result).unwrap().contains(TOKEN),
        "the secret leaked into the result"
    );
    assert_eq!(output["stdout_stderr_redaction"], "granted_credentials");

    let network = &output["network"];
    assert_eq!(network["policy"], "allowlist");
    assert_eq!(network["credentials_granted"], json!(["TEST_TOKEN"]));
    let egress = network["egress"].as_array().unwrap();
    assert!(
        egress
            .iter()
            .any(|e| e["host"] == "localhost" && e["allowed"] == true && e["bytes_down"] == 5),
        "{network:#}"
    );
    assert!(
        egress
            .iter()
            .any(|e| e["host"] == "example.com" && e["allowed"] == false),
        "{network:#}"
    );

    let files: Vec<&str> = output["workspace_output_files"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| f["path"].as_str().unwrap())
        .collect();
    assert_eq!(files, ["ok.txt"]);
    assert!(output["rejected_output_entries"]
        .as_array()
        .unwrap()
        .iter()
        .any(|r| r["path"] == "leak.txt" && r["reason"] == "contained a granted credential"));
    let staged = artifact_staging_dir(&root.join("artifacts"), "EGRESS-ALLOW");
    assert!(!staged.join("leak.txt").exists());
    let _ = fs::remove_dir_all(root);
}

#[tokio::test]
async fn allowlists_and_credentials_need_egress_and_static_forwarders() {
    let root = temp("refuse");
    let Some(runner) = base_runner(&root) else {
        return;
    };
    let allow = NetworkPolicy::Allowlist {
        hosts: vec!["api.example.test".into()],
    };
    let error = execute_with_runner(
        &runner,
        &request("EGRESS-NO-CONFIG", "true", json!({}), allow.clone()),
    )
    .await
    .unwrap_err();
    assert_eq!(error.kind, RunnerErrorKind::UnsupportedPolicy);

    let error = base_runner(&root)
        .unwrap()
        .with_egress(egress(PathBuf::from("/bin/bash")))
        .err()
        .unwrap();
    assert!(
        error.message.contains("statically linked"),
        "{}",
        error.message
    );

    let runner = base_runner(&root)
        .unwrap()
        .with_egress(egress(static_forwarder()))
        .unwrap();
    let deny = execute_with_runner(
        &runner,
        &request(
            "EGRESS-CRED-DENY",
            "true",
            json!({"credentials": ["TEST_TOKEN"]}),
            NetworkPolicy::DenyAll,
        ),
    )
    .await
    .unwrap_err();
    assert!(
        deny.message.contains("require an allowlist"),
        "{}",
        deny.message
    );
    let unknown = execute_with_runner(
        &runner,
        &request(
            "EGRESS-CRED-UNKNOWN",
            "true",
            json!({"credentials": ["OTHER"]}),
            allow,
        ),
    )
    .await
    .unwrap_err();
    assert!(
        unknown.message.contains("not granted"),
        "{}",
        unknown.message
    );
    let _ = fs::remove_dir_all(root);
}
