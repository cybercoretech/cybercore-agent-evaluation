use serde_json::Value;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_workbench");

fn example() -> Value {
    load_example("cat-smoke.json")
}

fn load_example(name: &str) -> Value {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("evaluations")
        .join(name);
    serde_json::from_str(&fs::read_to_string(path).unwrap()).unwrap()
}

struct TempDir(PathBuf);

impl TempDir {
    fn new(name: &str) -> Self {
        let path =
            std::env::temp_dir().join(format!("workbench-cli-test-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).unwrap();
        Self(path)
    }

    fn file(&self, name: &str, value: &Value) -> PathBuf {
        let path = self.0.join(name);
        fs::write(&path, serde_json::to_string_pretty(value).unwrap()).unwrap();
        path
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// Runs the binary with a clean store environment.
fn workbench(args: &[&str], env: &[(&str, &Path)]) -> Output {
    let mut command = Command::new(BIN);
    command
        .args(args)
        .env_remove("WORKBENCH_STORE")
        .env_remove("XDG_DATA_HOME");
    for (key, value) in env {
        command.env(key, value);
    }
    command.output().unwrap()
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

#[test]
fn help_and_version_describe_the_real_commands() {
    let help = workbench(&["--help"], &[]);
    assert!(help.status.success());
    let text = stdout(&help);
    for needle in [
        "run",
        "status",
        "--store",
        "WORKBENCH_STORE",
        "XDG_DATA_HOME",
        "Exit codes",
    ] {
        assert!(text.contains(needle), "help is missing {needle}");
    }
    let version = workbench(&["--version"], &[]);
    assert_eq!(
        stdout(&version).trim(),
        format!("workbench {}", env!("CARGO_PKG_VERSION"))
    );
}

#[test]
fn usage_errors_exit_2() {
    assert_eq!(workbench(&["frobnicate"], &[]).status.code(), Some(2));
    assert_eq!(workbench(&["run"], &[]).status.code(), Some(2));
}

#[test]
fn dry_run_validates_and_writes_nothing() {
    let dir = TempDir::new("dry-run");
    let file = dir.file("eval.json", &example());
    let store = dir.0.join("store");
    let output = workbench(
        &[
            "run",
            file.to_str().unwrap(),
            "--dry-run",
            "--store",
            store.to_str().unwrap(),
        ],
        &[],
    );
    assert!(output.status.success(), "{}", stderr(&output));
    let text = stdout(&output);
    assert!(text.contains("dry run: nothing will be executed or written"));
    assert!(text.contains("EVAL-CAT-SMOKE"));
    assert!(text.contains("TRIAL-GREETING"));
    assert!(!store.exists(), "dry run must not create the store");
}

/// (case name, mutation of the example file, expected error text)
type BadFileCase = (&'static str, Box<dyn Fn(&mut Value)>, &'static str);

#[test]
fn bad_files_name_the_file_and_the_field() {
    let dir = TempDir::new("bad-files");
    let cases: Vec<BadFileCase> = vec![
        (
            "unknown field",
            Box::new(|v| {
                v["surprise"] = Value::Bool(true);
            }),
            "unknown field `surprise`",
        ),
        (
            "format",
            Box::new(|v| {
                v["format"] = "v9".into();
            }),
            "format must be",
        ),
        (
            "executable",
            Box::new(|v| {
                v["schedule"]["trials"][0]["task_input"]["executable_id"] = "sh".into();
            }),
            "task_input.executable_id \"sh\" is not in runner.allowed_executables",
        ),
        (
            "verifier",
            Box::new(|v| {
                v["verifier"]["expected"]["TRIAL-GHOST"] = "x".into();
            }),
            "verifier.expected.TRIAL-GHOST",
        ),
        (
            "relative path",
            Box::new(|v| {
                v["runner"]["allowed_executables"]["cat"] = "cat".into();
            }),
            "runner.allowed_executables.cat must be an absolute path",
        ),
        (
            "schedule",
            Box::new(|v| {
                v["schedule"]["max_active_runs"] = 0.into();
            }),
            "max_active_runs must be greater than zero",
        ),
        (
            "task revision type",
            Box::new(|v| {
                v["task_revisions"][0]["record_type"] = "run".into();
            }),
            "task_revisions[0].record_type must be \"task_revision\"",
        ),
        (
            "task revision id",
            Box::new(|v| {
                v["task_revisions"][0]["record_id"] = "TASK-ELSEWHERE".into();
            }),
            "is not the task_revision_ref of any trial",
        ),
        (
            "task revision schema",
            Box::new(|v| {
                v["task_revisions"][0]["acceptance_criteria"][0]["criticality"] = "vital".into();
            }),
            "task_revisions[0]: invalid record",
        ),
    ];
    for (name, mutate, expected) in cases {
        let mut value = example();
        mutate(&mut value);
        let file = dir.file(&format!("{}.json", name.replace(' ', "-")), &value);
        let output = workbench(
            &[
                "run",
                file.to_str().unwrap(),
                "--dry-run",
                "--store",
                dir.0.join("s").to_str().unwrap(),
            ],
            &[],
        );
        assert_eq!(output.status.code(), Some(1), "{name}");
        let message = stderr(&output);
        assert!(
            message.contains(file.to_str().unwrap()),
            "{name}: no path in {message}"
        );
        assert!(
            message.contains(expected),
            "{name}: expected {expected:?} in {message}"
        );
    }
}

#[test]
fn store_location_follows_flag_then_env_then_xdg() {
    let dir = TempDir::new("layering");
    let file = dir.file("eval.json", &example());
    let file = file.to_str().unwrap();
    let flag = dir.0.join("from-flag");
    let env = dir.0.join("from-env");
    let xdg = dir.0.join("xdg");
    let store_line = |output: &Output| {
        stdout(output)
            .lines()
            .find(|l| l.starts_with("store"))
            .unwrap()
            .to_string()
    };

    let out = workbench(
        &["run", file, "--dry-run", "--store", flag.to_str().unwrap()],
        &[("WORKBENCH_STORE", &env), ("XDG_DATA_HOME", &xdg)],
    );
    assert!(store_line(&out).ends_with(flag.to_str().unwrap()));

    let out = workbench(
        &["run", file, "--dry-run"],
        &[("WORKBENCH_STORE", &env), ("XDG_DATA_HOME", &xdg)],
    );
    assert!(store_line(&out).ends_with(env.to_str().unwrap()));

    let out = workbench(&["run", file, "--dry-run"], &[("XDG_DATA_HOME", &xdg)]);
    assert!(store_line(&out).ends_with(xdg.join("cybercore-workbench").to_str().unwrap()));
}

#[test]
fn status_is_read_only_and_fails_without_a_store() {
    let dir = TempDir::new("status");
    let store = dir.0.join("missing");
    let output = workbench(
        &[
            "status",
            "EVAL-CAT-SMOKE",
            "--store",
            store.to_str().unwrap(),
        ],
        &[],
    );
    assert_eq!(output.status.code(), Some(1));
    assert!(stderr(&output).contains("no workbench store"));
    assert!(!store.exists(), "status must not create a store");
}

/// Real sandboxed execution. Skips (loudly) where Bubblewrap cannot create
/// namespaces, e.g. hosts that restrict unprivileged user namespaces.
#[test]
fn run_executes_records_and_resumes_idempotently() {
    let dir = TempDir::new("real-run");
    let file = dir.file("eval.json", &example());
    let file = file.to_str().unwrap();
    let store = dir.0.join("store");
    let store_arg = store.to_str().unwrap();

    let first = workbench(&["run", file, "--store", store_arg], &[]);
    if !first.status.success() && stderr(&first).contains("runner:") {
        // CI sets WORKBENCH_REQUIRE_BWRAP=1 on a job that installs Bubblewrap,
        // so an unusable sandbox fails there instead of passing as a skip.
        assert!(
            std::env::var_os("WORKBENCH_REQUIRE_BWRAP").is_none(),
            "WORKBENCH_REQUIRE_BWRAP is set but Bubblewrap is unavailable: {}",
            stderr(&first).trim()
        );
        eprintln!(
            "SKIPPED: Bubblewrap is unavailable here: {}",
            stderr(&first).trim()
        );
        return;
    }
    assert!(first.status.success(), "{}", stderr(&first));
    let text = stdout(&first);
    assert!(text.contains("created EVAL-CAT-SMOKE"));
    assert!(text.contains("executed 2 attempt(s), up to 2 at once"));
    assert!(text.contains("latest attempt per trial: 1 fail, 1 pass"));

    let again = workbench(&["run", file, "--store", store_arg], &[]);
    assert!(again.status.success(), "{}", stderr(&again));
    assert!(stdout(&again).contains("resuming EVAL-CAT-SMOKE"));
    assert!(stdout(&again).contains("executed 0 attempt(s)"));

    let status = workbench(
        &["status", "EVAL-CAT-SMOKE", "--json"],
        &[("WORKBENCH_STORE", &store)],
    );
    assert!(status.status.success(), "{}", stderr(&status));
    let report: Value = serde_json::from_str(&stdout(&status)).unwrap();
    let outcomes: Vec<&str> = report["trials"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["runs"][0]["run_outcome"].as_str().unwrap())
        .collect();
    assert_eq!(outcomes, vec!["pass", "fail"]);

    // The same evaluation ID with a changed schedule is refused.
    let mut changed = example();
    changed["schedule"]["max_active_runs"] = 3.into();
    let changed_file = dir.file("changed.json", &changed);
    let refused = workbench(
        &[
            "run",
            changed_file.to_str().unwrap(),
            "--dry-run",
            "--store",
            store_arg,
        ],
        &[],
    );
    assert_eq!(refused.status.code(), Some(1));
    assert!(stderr(&refused).contains("different schedule"));
}

#[test]
fn file_checks_are_validated_against_the_schedule() {
    let dir = TempDir::new("file-checks");
    let cases: Vec<BadFileCase> = vec![
        (
            "not collected",
            Box::new(|v| {
                v["schedule"]["trials"][0]["task_input"]["collect_outputs"] = false.into();
            }),
            "checks a file, but schedule.trials[TRIAL-GOOD-REPORT].task_input.collect_outputs is not true",
        ),
        (
            "escaping path",
            Box::new(|v| {
                v["verifier"]["checks"]["TRIAL-GOOD-REPORT"][0]["path"] = "../etc/passwd".into();
            }),
            "must be relative to /workspace/output without '..'",
        ),
        (
            "repeated criterion",
            Box::new(|v| {
                v["verifier"]["checks"]["TRIAL-GOOD-REPORT"][1]["criterion_id"] = "report-written".into();
            }),
            "criterion_id \"report-written\" is repeated",
        ),
        (
            "zero artifact budget",
            Box::new(|v| {
                v["schedule"]["trials"][0]["budget"]["max_artifact_bytes"] = 0.into();
            }),
            "budget.max_artifact_bytes must be between 1 and runner.limits.max_artifact_bytes",
        ),
        (
            "unknown check kind",
            Box::new(|v| {
                v["verifier"]["checks"]["TRIAL-GOOD-REPORT"][0]["kind"] = "file_smells_ok".into();
            }),
            "unknown variant `file_smells_ok`",
        ),
    ];
    for (name, mutate, expected) in cases {
        let mut value = load_example("report-files.json");
        mutate(&mut value);
        let file = dir.file(&format!("{}.json", name.replace(' ', "-")), &value);
        let output = workbench(
            &[
                "run",
                file.to_str().unwrap(),
                "--dry-run",
                "--store",
                dir.0.join("s").to_str().unwrap(),
            ],
            &[],
        );
        assert_eq!(output.status.code(), Some(1), "{name}");
        let message = stderr(&output);
        assert!(
            message.contains(expected),
            "{name}: expected {expected:?} in {message}"
        );
    }
}

/// Real sandboxed file collection and file checks through the CLI.
#[test]
fn file_checks_judge_what_the_agent_wrote() {
    let dir = TempDir::new("report-run");
    let file = dir.file("eval.json", &load_example("report-files.json"));
    let store = dir.0.join("store");
    let output = workbench(
        &[
            "run",
            file.to_str().unwrap(),
            "--store",
            store.to_str().unwrap(),
        ],
        &[],
    );
    if !output.status.success() && stderr(&output).contains("runner:") {
        assert!(
            std::env::var_os("WORKBENCH_REQUIRE_BWRAP").is_none(),
            "WORKBENCH_REQUIRE_BWRAP is set but Bubblewrap is unavailable: {}",
            stderr(&output).trim()
        );
        eprintln!(
            "SKIPPED: Bubblewrap is unavailable here: {}",
            stderr(&output).trim()
        );
        return;
    }
    assert!(output.status.success(), "{}", stderr(&output));
    let status = workbench(
        &["status", "EVAL-REPORT-FILES", "--json"],
        &[("WORKBENCH_STORE", &store)],
    );
    let report: Value = serde_json::from_str(&stdout(&status)).unwrap();
    let outcome = |i: usize| {
        let run = &report["trials"][i]["runs"][0];
        (
            run["verification_state"].as_str().unwrap().to_string(),
            run["run_outcome"].as_str().unwrap().to_string(),
        )
    };
    // Correct file -> pass. Wrong path (mandatory) + scratch file (minor) -> fail.
    assert_eq!(outcome(0), ("verified_pass".into(), "pass".into()));
    assert_eq!(outcome(1), ("mixed".into(), "fail".into()));
    // Staged copies are cleaned up once recorded.
    let staging = store.join("runner-artifacts");
    assert_eq!(
        fs::read_dir(&staging).unwrap().count(),
        0,
        "staging left behind"
    );
}

#[test]
fn podman_runner_specs_are_validated() {
    let dir = TempDir::new("podman-spec");
    let cases: Vec<BadFileCase> = vec![
        (
            "unknown image",
            Box::new(|v| {
                v["schedule"]["trials"][0]["task_input"]["image_id"] = "alpine".into();
            }),
            "task_input.image_id \"alpine\" is not in runner.images",
        ),
        (
            "relative in-image path",
            Box::new(|v| {
                v["runner"]["allowed_executables"]["sh"] = "bin/sh".into();
            }),
            "runner.allowed_executables.sh must be an absolute path inside the image",
        ),
        (
            "no images",
            Box::new(|v| {
                v["runner"]["images"] = serde_json::json!({});
            }),
            "runner.images must list at least one image",
        ),
        (
            "unknown resource",
            Box::new(|v| {
                v["runner"]["resources"]["gpu"] = 1.into();
            }),
            "unknown field `gpu`",
        ),
    ];
    for (name, mutate, expected) in cases {
        let mut value = load_example("container-report.json");
        mutate(&mut value);
        let file = dir.file(&format!("{}.json", name.replace(' ', "-")), &value);
        let output = workbench(
            &[
                "run",
                file.to_str().unwrap(),
                "--dry-run",
                "--store",
                dir.0.join("s").to_str().unwrap(),
            ],
            &[],
        );
        assert_eq!(output.status.code(), Some(1), "{name}");
        let message = stderr(&output);
        assert!(
            message.contains(expected),
            "{name}: expected {expected:?} in {message}"
        );
    }
}

/// Real rootless-Podman run through the CLI. Skips (loudly) without podman or
/// the ubuntu:22.04 image, unless WORKBENCH_REQUIRE_PODMAN is set.
#[test]
fn podman_runner_enforces_limits_and_checks_files() {
    let dir = TempDir::new("podman-run");
    let file = dir.file("eval.json", &load_example("container-report.json"));
    let store = dir.0.join("store");
    let output = workbench(
        &[
            "run",
            file.to_str().unwrap(),
            "--store",
            store.to_str().unwrap(),
        ],
        &[],
    );
    if !output.status.success() && stderr(&output).contains("runner:") {
        assert!(
            std::env::var_os("WORKBENCH_REQUIRE_PODMAN").is_none(),
            "WORKBENCH_REQUIRE_PODMAN is set but the container runner is unavailable: {}",
            stderr(&output).trim()
        );
        eprintln!(
            "SKIPPED: container runner unavailable here: {}",
            stderr(&output).trim()
        );
        return;
    }
    assert!(output.status.success(), "{}", stderr(&output));
    let status = workbench(
        &["status", "EVAL-CONTAINER-REPORT", "--json"],
        &[("WORKBENCH_STORE", &store)],
    );
    let report: Value = serde_json::from_str(&stdout(&status)).unwrap();
    let outcome = |i: usize| {
        let run = &report["trials"][i]["runs"][0];
        (
            run["execution_status"].as_str().unwrap().to_string(),
            run["run_outcome"].as_str().unwrap().to_string(),
        )
    };
    assert_eq!(outcome(0), ("finished".into(), "pass".into()));
    assert_eq!(outcome(1), ("finished".into(), "fail".into()));
    // The memory hog is stopped by the cgroup limit: an execution error,
    // not a task failure.
    assert_eq!(outcome(2), ("errored".into(), "not_scored".into()));
}

#[test]
fn model_judge_specs_are_validated() {
    let dir = TempDir::new("judge-spec");
    let cases: Vec<BadFileCase> = vec![
        (
            "no consent",
            Box::new(|v| {
                v["verifier"]["consent_to_send_outputs"] = false.into();
            }),
            "verifier.consent_to_send_outputs must be true",
        ),
        (
            "bad effort",
            Box::new(|v| {
                v["verifier"]["effort"] = "extreme".into();
            }),
            "verifier.effort \"extreme\" must be one of",
        ),
        (
            "file without collection",
            Box::new(|v| {
                v["verifier"]["criteria"]["TRIAL-GREETING"][0]["file"] = "out.txt".into();
            }),
            "judges a file, but schedule.trials[TRIAL-GREETING].task_input.collect_outputs is not true",
        ),
        (
            "empty rubric",
            Box::new(|v| {
                v["verifier"]["criteria"]["TRIAL-GREETING"][0]["rubric"] = " ".into();
            }),
            "rubric must not be empty",
        ),
        (
            "credential in file",
            Box::new(|v| {
                v["verifier"]["api_key"] = "sk-not-here".into();
            }),
            "unknown field `api_key`",
        ),
    ];
    for (name, mutate, expected) in cases {
        let mut value = load_example("model-judge.json");
        mutate(&mut value);
        let file = dir.file(&format!("{}.json", name.replace(' ', "-")), &value);
        let output = workbench(
            &[
                "run",
                file.to_str().unwrap(),
                "--dry-run",
                "--store",
                dir.0.join("s").to_str().unwrap(),
            ],
            &[],
        );
        assert_eq!(output.status.code(), Some(1), "{name}");
        let message = stderr(&output);
        assert!(
            message.contains(expected),
            "{name}: expected {expected:?} in {message}"
        );
    }
    let file = dir.file("ok.json", &load_example("model-judge.json"));
    let plan = workbench(
        &[
            "run",
            file.to_str().unwrap(),
            "--dry-run",
            "--store",
            dir.0.join("s").to_str().unwrap(),
        ],
        &[],
    );
    assert!(stdout(&plan).contains("model_judge (claude-opus-5-5, effort medium): judged outputs are sent to the Anthropic API"));
}

/// A one-request-at-a-time mock of the Messages API that judges by keyword,
/// so the whole CLI path runs with nothing leaving the machine.
fn mock_judge_api(pass_keyword: &'static str) -> String {
    use std::io::{BufRead, BufReader, Read, Write};
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(stream) = stream else { return };
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut length = 0;
            let mut line = String::new();
            loop {
                line.clear();
                if reader.read_line(&mut line).unwrap_or(0) == 0 || line.trim_end().is_empty() {
                    break;
                }
                if let Some(v) = line.to_ascii_lowercase().strip_prefix("content-length:") {
                    length = v.trim().parse().unwrap_or(0);
                }
            }
            let mut raw = vec![0_u8; length];
            let _ = reader.read_exact(&mut raw);
            let body: Value = serde_json::from_slice(&raw).unwrap_or(Value::Null);
            let prompt = body["messages"][0]["content"]
                .as_str()
                .unwrap_or("")
                .to_string();
            let verdict = if prompt.contains(pass_keyword) {
                "pass"
            } else {
                "fail"
            };
            let text = serde_json::json!({"verdict": verdict, "rationale": format!("mock judged {verdict}")}).to_string();
            let reply = serde_json::json!({
                "model": body["model"], "stop_reason": "end_turn",
                "content": [{"type": "text", "text": text}]
            })
            .to_string();
            let mut stream = stream;
            let _ = write!(
                stream,
                "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{reply}",
                reply.len()
            );
        }
    });
    url
}

#[test]
fn model_judge_verdicts_flow_through_a_real_run() {
    let dir = TempDir::new("judge-run");
    let file = dir.file("eval.json", &load_example("model-judge.json"));
    let store = dir.0.join("store");
    let api = mock_judge_api("hello from the sandbox");
    let output = Command::new(BIN)
        .args([
            "run",
            file.to_str().unwrap(),
            "--store",
            store.to_str().unwrap(),
        ])
        .env_remove("WORKBENCH_STORE")
        .env_remove("ANTHROPIC_AUTH_TOKEN")
        .env("ANTHROPIC_BASE_URL", &api)
        .env("ANTHROPIC_API_KEY", "test-key-for-local-mock")
        .output()
        .unwrap();
    if !output.status.success() && stderr(&output).contains("runner:") {
        assert!(
            std::env::var_os("WORKBENCH_REQUIRE_BWRAP").is_none(),
            "WORKBENCH_REQUIRE_BWRAP is set but Bubblewrap is unavailable: {}",
            stderr(&output).trim()
        );
        eprintln!(
            "SKIPPED: Bubblewrap is unavailable here: {}",
            stderr(&output).trim()
        );
        return;
    }
    assert!(output.status.success(), "{}", stderr(&output));
    let status = workbench(
        &["status", "EVAL-MODEL-JUDGE", "--json"],
        &[("WORKBENCH_STORE", &store)],
    );
    let report: Value = serde_json::from_str(&stdout(&status)).unwrap();
    let outcome = |i: usize| {
        report["trials"][i]["runs"][0]["run_outcome"]
            .as_str()
            .unwrap()
            .to_string()
    };
    // The mock passes the greeting and fails the numbers trial.
    assert_eq!(outcome(0), "pass");
    assert_eq!(outcome(1), "fail");
}

#[test]
fn composite_specs_are_validated() {
    let dir = TempDir::new("composite-spec");
    let cases: Vec<BadFileCase> = vec![
        (
            "duplicate criterion",
            Box::new(|v| {
                v["verifier"]["verifiers"][1]["criteria"]["TRIAL-GOOD-REPORT"][0]["criterion_id"] =
                    "report-written".into();
            }),
            "verifier.verifiers[0] and verifier.verifiers[1] both judge criterion \"report-written\" for trial TRIAL-GOOD-REPORT",
        ),
        (
            "sub-verifier error path",
            Box::new(|v| {
                v["verifier"]["verifiers"][1]["consent_to_send_outputs"] = false.into();
            }),
            "verifier.verifiers[1].consent_to_send_outputs must be true",
        ),
        (
            "nested composite",
            Box::new(|v| {
                let inner = v["verifier"].clone();
                v["verifier"]["verifiers"][1] = inner;
            }),
            "verifier.verifiers[1]: a composite verifier cannot contain another composite",
        ),
        (
            "empty composite",
            Box::new(|v| {
                v["verifier"]["verifiers"] = serde_json::json!([]);
            }),
            "verifier.verifiers must list at least one verifier",
        ),
    ];
    for (name, mutate, expected) in cases {
        let mut value = load_example("composite-report.json");
        mutate(&mut value);
        let file = dir.file(&format!("{}.json", name.replace(' ', "-")), &value);
        let output = workbench(
            &[
                "run",
                file.to_str().unwrap(),
                "--dry-run",
                "--store",
                dir.0.join("s").to_str().unwrap(),
            ],
            &[],
        );
        assert_eq!(output.status.code(), Some(1), "{name}");
        let message = stderr(&output);
        assert!(
            message.contains(expected),
            "{name}: expected {expected:?} in {message}"
        );
    }
    let file = dir.file("ok.json", &load_example("composite-report.json"));
    let plan = workbench(
        &[
            "run",
            file.to_str().unwrap(),
            "--dry-run",
            "--store",
            dir.0.join("s").to_str().unwrap(),
        ],
        &[],
    );
    assert!(stdout(&plan)
        .contains("composite of checks (deterministic) + model_judge (claude-opus-5-5"));
}

/// Exact checks and a (mocked) model judge in one real sandboxed run.
#[test]
fn composite_verdicts_flow_through_a_real_run() {
    let dir = TempDir::new("composite-run");
    let file = dir.file("eval.json", &load_example("composite-report.json"));
    let store = dir.0.join("store");
    let api = mock_judge_api("total: 3");
    let output = Command::new(BIN)
        .args([
            "run",
            file.to_str().unwrap(),
            "--store",
            store.to_str().unwrap(),
        ])
        .env_remove("WORKBENCH_STORE")
        .env_remove("ANTHROPIC_AUTH_TOKEN")
        .env("ANTHROPIC_BASE_URL", &api)
        .env("ANTHROPIC_API_KEY", "test-key-for-local-mock")
        .output()
        .unwrap();
    if !output.status.success() && stderr(&output).contains("runner:") {
        assert!(
            std::env::var_os("WORKBENCH_REQUIRE_BWRAP").is_none(),
            "WORKBENCH_REQUIRE_BWRAP is set but Bubblewrap is unavailable: {}",
            stderr(&output).trim()
        );
        eprintln!(
            "SKIPPED: Bubblewrap is unavailable here: {}",
            stderr(&output).trim()
        );
        return;
    }
    assert!(output.status.success(), "{}", stderr(&output));
    let status = workbench(
        &["status", "EVAL-COMPOSITE-REPORT", "--json"],
        &[("WORKBENCH_STORE", &store)],
    );
    let report: Value = serde_json::from_str(&stdout(&status)).unwrap();
    let run = |i: usize| {
        let r = &report["trials"][i]["runs"][0];
        (
            r["verification_state"].as_str().unwrap().to_string(),
            r["run_outcome"].as_str().unwrap().to_string(),
        )
    };
    // Good report: every exact check and the judged clarity criterion pass.
    assert_eq!(run(0), ("verified_pass".into(), "pass".into()));
    // Sloppy report: the mandatory exact check fails; the judge has no file
    // at out/report.txt to read (unavailable), so the state is mixed.
    assert_eq!(run(1), ("mixed".into(), "fail".into()));
}
