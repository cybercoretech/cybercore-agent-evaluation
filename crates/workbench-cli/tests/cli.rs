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

#[test]
fn egress_specs_are_validated() {
    let dir = TempDir::new("egress-spec");
    let cases: Vec<BadFileCase> = vec![
        (
            "allowlist without egress",
            Box::new(|v| {
                v["runner"].as_object_mut().unwrap().remove("egress");
            }),
            "schedule.trials[TRIAL-REACH-ALLOWED-HOST].network_policy is an allowlist, which needs runner.type \"podman\" with runner.egress",
        ),
        (
            "credentials without allowlist",
            Box::new(|v| {
                v["schedule"]["trials"][0]["network_policy"] = serde_json::json!({"mode": "deny_all"});
            }),
            "schedule.trials[TRIAL-REACH-ALLOWED-HOST].task_input.credentials need an allowlist network_policy",
        ),
        (
            "ungranted credential",
            Box::new(|v| {
                v["schedule"]["trials"][0]["task_input"]["credentials"] = serde_json::json!(["OTHER_KEY"]);
            }),
            "lists \"OTHER_KEY\", which is not in runner.egress.credentials",
        ),
        (
            "secret value in the file",
            Box::new(|v| {
                v["runner"]["egress"]["credentials"]["ANTHROPIC_API_KEY"] = serde_json::json!({"value": "sk-..."});
            }),
            "unknown field `value`",
        ),
    ];
    for (name, mutate, expected) in cases {
        let mut value = load_example("container-egress.json");
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

/// Builds the egress forwarder statically, once per test binary.
fn static_forwarder() -> PathBuf {
    static PATH: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();
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
            .unwrap();
        assert!(status.success(), "building the static forwarder failed");
        target_dir.join("x86_64-unknown-linux-gnu/release/cybercore-egress-forwarder")
    })
    .clone()
}

/// A TLS stand-in on the host loopback: reads one TLS record, answers PONG.
fn tls_stub() -> u16 {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || {
        use std::io::{Read, Write};
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

/// The egress example, pointed at a local TLS stub instead of the internet,
/// run for real through Podman; the granted token must never reach the store.
#[test]
fn egress_example_runs_against_a_local_stub_without_leaking_the_token() {
    const TOKEN: &str = "wb-egress-test-token-0123456789";
    let dir = TempDir::new("egress-run");
    let port = tls_stub();
    let mut value = load_example("container-egress.json");
    let trial = &mut value["schedule"]["trials"][0];
    trial["network_policy"]["hosts"] = serde_json::json!(["localhost"]);
    let script = trial["task_input"]["args"][1]
        .as_str()
        .unwrap()
        .replace("api.anthropic.com:443", &format!("localhost:{port}"))
        + "\necho \"leak:$ANTHROPIC_API_KEY\"";
    trial["task_input"]["args"][1] = script.into();
    trial["task_input"]["environment"]["HELLO"] = hello_escapes("localhost").into();
    let egress = &mut value["runner"]["egress"];
    egress["ports"] = serde_json::json!([port]);
    egress["allow_private_destinations"] = true.into();
    egress["forwarder_path"] = static_forwarder().to_str().unwrap().into();
    egress["credentials"]["ANTHROPIC_API_KEY"]["from_env"] = "WB_EGRESS_TEST_TOKEN".into();
    let file = dir.file("eval.json", &value);
    let store = dir.0.join("store");
    let output = Command::new(BIN)
        .args([
            "run",
            file.to_str().unwrap(),
            "--store",
            store.to_str().unwrap(),
        ])
        .env_remove("WORKBENCH_STORE")
        .env("WB_EGRESS_TEST_TOKEN", TOKEN)
        .output()
        .unwrap();
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
        &["status", "EVAL-CONTAINER-EGRESS", "--json"],
        &[("WORKBENCH_STORE", &store)],
    );
    let report: Value = serde_json::from_str(&stdout(&status)).unwrap();
    let run = &report["trials"][0]["runs"][0];
    assert_eq!(run["verification_state"], "verified_pass", "{report:#}");
    assert_eq!(run["run_outcome"], "pass");

    // Missing host variable: a clear error, never an empty secret.
    let missing = Command::new(BIN)
        .args([
            "run",
            file.to_str().unwrap(),
            "--store",
            dir.0.join("store2").to_str().unwrap(),
        ])
        .env_remove("WORKBENCH_STORE")
        .env_remove("WB_EGRESS_TEST_TOKEN")
        .output()
        .unwrap();
    assert!(
        stderr(&missing)
            .contains("reads host environment variable WB_EGRESS_TEST_TOKEN, which is not set"),
        "{}",
        stderr(&missing)
    );

    let mut stack = vec![store];
    while let Some(path) = stack.pop() {
        for entry in fs::read_dir(&path).unwrap().flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else {
                let bytes = fs::read(&path).unwrap();
                assert!(
                    !bytes.windows(TOKEN.len()).any(|w| w == TOKEN.as_bytes()),
                    "token leaked into {}",
                    path.display()
                );
            }
        }
    }
}

/// Twelve one-trial tasks; the first `passes` produce the expected output.
fn twelve_task_evaluation(id: &str, passes: usize) -> Value {
    let mut value = load_example("cat-smoke.json");
    value["schedule"]["evaluation_ref"]["record_id"] = id.into();
    value["schedule"]["plan_ref"]["record_id"] = format!("PLAN-{id}").into();
    let template = value["schedule"]["trials"][0].clone();
    let mut trials = Vec::new();
    let mut expected = serde_json::Map::new();
    for i in 0..12 {
        let mut trial = template.clone();
        trial["trial_ref"]["record_id"] = format!("TRIAL-{i}").into();
        trial["task_revision_ref"]["record_id"] = format!("TASK-{i}").into();
        let content = if i < passes {
            format!("answer {i}\n")
        } else {
            "wrong\n".into()
        };
        trial["task_input"]["files"][0]["content"] = content.into();
        trials.push(trial);
        expected.insert(format!("TRIAL-{i}"), format!("answer {i}\n").into());
    }
    value["schedule"]["trials"] = trials.into();
    value["verifier"] = serde_json::json!({"type": "stdout_equals", "expected": expected});
    value["task_revisions"] = serde_json::json!([]);
    value
}

#[test]
fn summary_and_compare_over_real_runs() {
    let dir = TempDir::new("summary");
    let store = dir.0.join("store");
    for (id, passes) in [("EVAL-BASE", 4), ("EVAL-TREAT", 11)] {
        let file = dir.file(&format!("{id}.json"), &twelve_task_evaluation(id, passes));
        let output = workbench(
            &["run", file.to_str().unwrap()],
            &[("WORKBENCH_STORE", &store)],
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
    }

    let summary = workbench(
        &["summary", "EVAL-BASE", "--json"],
        &[("WORKBENCH_STORE", &store)],
    );
    assert!(summary.status.success(), "{}", stderr(&summary));
    let base: Value = serde_json::from_str(&stdout(&summary)).unwrap();
    assert_eq!(
        (base["passes"].as_u64(), base["denominator"].as_u64()),
        (Some(4), Some(12))
    );
    assert_eq!(base["status"], "computed");
    assert_eq!(base["interval"]["method"], "wilson");
    let (lower, upper) = (
        base["interval"]["lower"].as_f64().unwrap(),
        base["interval"]["upper"].as_f64().unwrap(),
    );
    assert!(
        (lower - 0.1381).abs() < 1e-3 && (upper - 0.6094).abs() < 1e-3,
        "{lower} {upper}"
    );
    let again = workbench(
        &["summary", "EVAL-BASE", "--json"],
        &[("WORKBENCH_STORE", &store)],
    );
    let again: Value = serde_json::from_str(&stdout(&again)).unwrap();
    assert_eq!(
        again["metric_result_id"], base["metric_result_id"],
        "unchanged results reuse their record"
    );

    let compare = workbench(
        &["compare", "EVAL-BASE", "EVAL-TREAT", "--json"],
        &[("WORKBENCH_STORE", &store)],
    );
    assert!(compare.status.success(), "{}", stderr(&compare));
    let report: Value = serde_json::from_str(&stdout(&compare)).unwrap();
    let comparison = &report["comparison"];
    assert_eq!(comparison["status"], "comparable", "{report:#}");
    assert_eq!(comparison["design"], "blocked_paired");
    assert_eq!(comparison["matched_tasks"], 12);
    assert!((comparison["difference"].as_f64().unwrap() - 7.0 / 12.0).abs() < 1e-9);
    assert!((comparison["p_value"].as_f64().unwrap() - 0.015625).abs() < 1e-9);
    assert_eq!(comparison["direction"], "favors_treatment");

    let text = workbench(
        &["compare", "EVAL-BASE", "EVAL-TREAT"],
        &[("WORKBENCH_STORE", &store)],
    );
    assert!(
        stdout(&text).contains("difference  +0.583"),
        "{}",
        stdout(&text)
    );

    let unknown = workbench(&["summary", "EVAL-NOPE"], &[("WORKBENCH_STORE", &store)]);
    assert_eq!(unknown.status.code(), Some(1));
    assert!(
        stderr(&unknown).contains("unknown evaluation EVAL-NOPE"),
        "{}",
        stderr(&unknown)
    );
}

fn declare(value: &mut Value, baseline: &str, alpha: f64) {
    value["analysis_plan"] = serde_json::json!({
        "decision_question": "Does the new agent pass more tasks than the baseline?",
        "comparisons": [{
            "comparison_id": "new-vs-base",
            "baseline_evaluation_id": baseline,
            "alpha": alpha,
            "minimum_matched_tasks": 10,
            "practical_significance_threshold": 0.2
        }]
    });
}

#[test]
fn predeclared_comparisons_are_frozen_and_confirmatory() {
    let dir = TempDir::new("predeclared");
    let store = dir.0.join("store");
    let env = [("WORKBENCH_STORE", store.as_path())];
    let run = |name: &str, value: &Value| {
        let file = dir.file(name, value);
        workbench(&["run", file.to_str().unwrap()], &env)
    };

    let base = run("base.json", &twelve_task_evaluation("EVAL-BASE", 4));
    if !base.status.success() && stderr(&base).contains("runner:") {
        assert!(
            std::env::var_os("WORKBENCH_REQUIRE_BWRAP").is_none(),
            "WORKBENCH_REQUIRE_BWRAP is set but Bubblewrap is unavailable: {}",
            stderr(&base).trim()
        );
        eprintln!(
            "SKIPPED: Bubblewrap is unavailable here: {}",
            stderr(&base).trim()
        );
        return;
    }
    assert!(base.status.success(), "{}", stderr(&base));

    // Declared before its first run: frozen, then confirmatory.
    let mut treat = twelve_task_evaluation("EVAL-TREAT", 11);
    declare(&mut treat, "EVAL-BASE", 0.05);
    let output = run("treat.json", &treat);
    assert!(output.status.success(), "{}", stderr(&output));
    assert!(
        stdout(&output).contains("froze analysis plan PLAN-EVAL-TREAT"),
        "{}",
        stdout(&output)
    );
    let rerun = run("treat.json", &treat);
    assert!(
        stdout(&rerun).contains("analysis plan PLAN-EVAL-TREAT unchanged"),
        "{}",
        stdout(&rerun)
    );

    let compare = workbench(&["compare", "EVAL-BASE", "EVAL-TREAT", "--json"], &env);
    assert!(compare.status.success(), "{}", stderr(&compare));
    let report: Value = serde_json::from_str(&stdout(&compare)).unwrap();
    let c = &report["comparison"];
    assert_eq!(c["inferential_status"], "confirmatory", "{report:#}");
    assert_eq!(c["declared_comparison_id"], "new-vs-base");
    assert_eq!(c["direction"], "favors_treatment");
    assert_eq!(c["practical_significance"], "meaningful");
    let text = workbench(&["compare", "EVAL-BASE", "EVAL-TREAT"], &env);
    assert!(
        stdout(&text).contains("inference   confirmatory (predeclared as new-vs-base)"),
        "{}",
        stdout(&text)
    );

    // A frozen plan cannot change.
    declare(&mut treat, "EVAL-BASE", 0.01);
    let changed = run("treat-changed.json", &treat);
    assert_eq!(changed.status.code(), Some(1));
    assert!(
        stderr(&changed).contains("was frozen at") && stderr(&changed).contains("cannot change"),
        "{}",
        stderr(&changed)
    );

    // Declared only after results existed: stays exploratory, with the reason.
    let late = twelve_task_evaluation("EVAL-LATE", 11);
    assert!(run("late.json", &late).status.success());
    let mut late_declared = late;
    declare(&mut late_declared, "EVAL-BASE", 0.05);
    assert!(run("late-declared.json", &late_declared).status.success());
    let compare = workbench(&["compare", "EVAL-BASE", "EVAL-LATE", "--json"], &env);
    let report: Value = serde_json::from_str(&stdout(&compare)).unwrap();
    assert_eq!(
        report["comparison"]["inferential_status"], "exploratory",
        "{report:#}"
    );
    let notes = report["comparison"]["limitations"].to_string();
    assert!(
        notes.contains("not confirmatory") && notes.contains("already had results"),
        "{notes}"
    );
}

#[test]
fn analysis_plans_are_validated() {
    let dir = TempDir::new("plan-spec");
    let cases: Vec<BadFileCase> = vec![
        (
            "alpha",
            Box::new(|v| v["analysis_plan"]["comparisons"][0]["alpha"] = 0.5.into()),
            "analysis_plan.comparisons[0].alpha must be greater than 0 and at most 0.2",
        ),
        (
            "minimum",
            Box::new(|v| v["analysis_plan"]["comparisons"][0]["minimum_matched_tasks"] = 3.into()),
            "minimum_matched_tasks must be at least 5",
        ),
        (
            "self",
            Box::new(|v| {
                v["analysis_plan"]["comparisons"][0]["baseline_evaluation_id"] = "EVAL-TREAT".into()
            }),
            "must be another evaluation",
        ),
        (
            "question",
            Box::new(|v| v["analysis_plan"]["decision_question"] = " ".into()),
            "decision_question must be 1 to 4096 characters",
        ),
        (
            "selection",
            Box::new(|v| v["analysis_plan"]["task_selection_method"] = "vibes".into()),
            "task_selection_method must be one of",
        ),
        (
            "fingerprints",
            Box::new(|v| {
                v["schedule"]["trials"][1]["system_fingerprint_ref"]["record_id"] =
                    "FP-OTHER".into()
            }),
            "share one system_fingerprint_ref",
        ),
    ];
    for (name, mutate, expected) in cases {
        let mut value = twelve_task_evaluation("EVAL-TREAT", 11);
        declare(&mut value, "EVAL-BASE", 0.05);
        mutate(&mut value);
        let file = dir.file(&format!("{name}.json"), &value);
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
        assert!(
            stderr(&output).contains(expected),
            "{name}: expected {expected:?} in {}",
            stderr(&output)
        );
    }
    let mut ok = twelve_task_evaluation("EVAL-TREAT", 11);
    declare(&mut ok, "EVAL-BASE", 0.05);
    let file = dir.file("ok.json", &ok);
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
    assert!(stdout(&plan).contains("new-vs-base: vs EVAL-BASE at alpha 0.05, at least 10 matched tasks, meaningful beyond ±0.2"), "{}", stdout(&plan));
}

/// Twelve tasks whose "agent" answers correctly and reports `tokens_scale`
/// times a base usage in `cybercore-usage.json` (ADR-0022).
fn usage_evaluation(id: &str, tokens_scale: u64) -> Value {
    let mut value = twelve_task_evaluation(id, 12);
    value["runner"]["allowed_executables"] = serde_json::json!({"sh": "/usr/bin/sh"});
    value["runner"]["limits"]["max_artifact_bytes"] = 65536.into();
    value["runner"]["limits"]["max_output_files"] = 4.into();
    for (i, trial) in value["schedule"]["trials"]
        .as_array_mut()
        .unwrap()
        .iter_mut()
        .enumerate()
    {
        let i = i as u64;
        let usage = format!(
            r#"{{"format":"cybercore-usage/v0","models":[{{"model_id":"model-a","input_tokens":{},"output_tokens":{}}}],"tool_calls":3}}"#,
            (1000 + i * 100) * tokens_scale,
            (200 + i * 10) * tokens_scale
        );
        trial["task_input"]["executable_id"] = "sh".into();
        trial["task_input"]["args"] = serde_json::json!([
            "-c",
            format!("printf 'answer {i}\\n'; printf '%s' '{usage}' > /workspace/output/cybercore-usage.json")
        ]);
        trial["task_input"]["collect_outputs"] = true.into();
        trial["budget"]["max_artifact_bytes"] = 65536.into();
    }
    value["pricing"] = serde_json::json!({
        "currency": "USD",
        "models": {"model-a": {"input_per_million_tokens": 3.0, "output_per_million_tokens": 15.0}}
    });
    value
}

#[test]
fn speed_and_cost_metrics_over_real_runs() {
    let dir = TempDir::new("usage");
    let store = dir.0.join("store");
    for (id, scale) in [("EVAL-COSTLY", 2), ("EVAL-LEAN", 1)] {
        let file = dir.file(&format!("{id}.json"), &usage_evaluation(id, scale));
        let output = workbench(
            &["run", file.to_str().unwrap()],
            &[("WORKBENCH_STORE", &store)],
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
    }

    let summary = workbench(
        &["summary", "EVAL-LEAN", "--json"],
        &[("WORKBENCH_STORE", &store)],
    );
    assert!(summary.status.success(), "{}", stderr(&summary));
    let lean: Value = serde_json::from_str(&stdout(&summary)).unwrap();
    assert_eq!(lean["status"], "computed");
    assert_eq!(lean["attempts"], 12, "each run is counted once");
    let metrics = lean["speed_and_cost"].as_array().unwrap();
    let [time, tokens, cost] = [&metrics[0], &metrics[1], &metrics[2]];
    assert_eq!(time["metric_id"], "workbench-wall-time");
    assert_eq!(time["status"], "computed", "{time}");
    assert!(time["estimate"].as_f64().unwrap() > 0.0);
    assert_eq!(time["self_reported"], false);
    // Tokens per trial: 1000 + 100i input and 200 + 10i output, i = 0..11.
    assert_eq!(tokens["status"], "computed", "{tokens}");
    assert_eq!(tokens["estimate"].as_f64(), Some(1200.0 + 110.0 * 5.5));
    assert_eq!(tokens["observed"], 12);
    assert_eq!(tokens["self_reported"], true);
    // Cost: 3 µ$ per input token and 15 µ$ per output token.
    let expected_total: f64 = (0..12)
        .map(|i| (1000.0 + 100.0 * i as f64) * 3.0 + (200.0 + 10.0 * i as f64) * 15.0)
        .sum();
    assert_eq!(cost["status"], "computed", "{cost}");
    assert_eq!(cost["currency"], "USD");
    assert_eq!(cost["total"].as_f64(), Some(expected_total));
    assert!(cost["limitations"]
        .as_array()
        .unwrap()
        .iter()
        .any(|l| l.as_str().unwrap().contains("not a bill")));

    let text = workbench(&["summary", "EVAL-LEAN"], &[("WORKBENCH_STORE", &store)]);
    let text = stdout(&text);
    assert!(text.contains("wall time   median"), "{text}");
    assert!(text.contains("tokens      mean 1805 tokens"), "{text}");
    assert!(text.contains("self-reported"), "{text}");
    assert!(
        text.contains(&format!("total {:.4} USD", expected_total / 1e6)),
        "{text}"
    );

    let compare = workbench(
        &["compare", "EVAL-COSTLY", "EVAL-LEAN", "--json"],
        &[("WORKBENCH_STORE", &store)],
    );
    assert!(compare.status.success(), "{}", stderr(&compare));
    let out: Value = serde_json::from_str(&stdout(&compare)).unwrap();
    let resources = out["speed_and_cost"].as_array().unwrap();
    assert_eq!(resources.len(), 3);
    for c in &resources[1..] {
        assert_eq!(c["status"], "comparable", "{c}");
        assert_eq!(c["direction"], "favors_treatment", "{c}");
        assert_eq!(c["inferential_status"], "exploratory");
        assert!((c["relative_difference"].as_f64().unwrap() + 0.5).abs() < 1e-9);
        assert!(c["comparison_result_id"]
            .as_str()
            .unwrap()
            .starts_with("CR-"));
    }
    // Repeating the command writes nothing new.
    let again = workbench(
        &["compare", "EVAL-COSTLY", "EVAL-LEAN", "--json"],
        &[("WORKBENCH_STORE", &store)],
    );
    let again: Value = serde_json::from_str(&stdout(&again)).unwrap();
    assert_eq!(again["speed_and_cost"], out["speed_and_cost"]);

    // Missing and invalid reports are counted, never imputed.
    let mut patchy = usage_evaluation("EVAL-PATCHY", 1);
    patchy["schedule"]["trials"][0]["task_input"]["args"][1] =
        "printf 'answer 0\\n'; printf 'not json' > /workspace/output/cybercore-usage.json".into();
    patchy["schedule"]["trials"][1]["task_input"]["args"][1] = "printf 'answer 1\\n'".into();
    let file = dir.file("patchy.json", &patchy);
    let output = workbench(
        &["run", file.to_str().unwrap()],
        &[("WORKBENCH_STORE", &store)],
    );
    assert!(output.status.success(), "{}", stderr(&output));
    let summary = workbench(
        &["summary", "EVAL-PATCHY", "--json"],
        &[("WORKBENCH_STORE", &store)],
    );
    let patchy: Value = serde_json::from_str(&stdout(&summary)).unwrap();
    let tokens = &patchy["speed_and_cost"][1];
    assert_eq!(
        (tokens["observed"].as_u64(), tokens["missing"].as_u64()),
        (Some(10), Some(2))
    );
    assert_eq!(tokens["status"], "descriptive_only");
    assert!(tokens["limitations"].as_array().unwrap().iter().any(|l| l
        .as_str()
        .unwrap()
        .contains("1 left no usage report and 1 left an invalid one")));
}

#[test]
fn pricing_is_validated() {
    let dir = TempDir::new("pricing-spec");
    let cases: Vec<BadFileCase> = vec![
        (
            "currency",
            Box::new(|v| v["pricing"]["currency"] = "dollars".into()),
            "pricing: currency must be three capital letters",
        ),
        (
            "negative",
            Box::new(|v| {
                v["pricing"]["models"]["model-a"]["input_per_million_tokens"] = (-1.0).into()
            }),
            "pricing: model-a.input_per_million_tokens must be between 0 and 10000",
        ),
        (
            "unknown",
            Box::new(|v| v["pricing"]["models"]["model-a"]["api_key"] = "x".into()),
            "unknown field `api_key`",
        ),
        (
            "no-outputs",
            Box::new(|v| {
                for trial in v["schedule"]["trials"].as_array_mut().unwrap() {
                    trial["task_input"]["collect_outputs"] = false.into();
                }
            }),
            "pricing is set, but no trial sets task_input.collect_outputs",
        ),
    ];
    for (name, mutate, expected) in cases {
        let mut value = usage_evaluation("EVAL-PRICED", 1);
        mutate(&mut value);
        let file = dir.file(&format!("{name}.json"), &value);
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
        assert!(
            stderr(&output).contains(expected),
            "{name}: expected {expected:?} in {}",
            stderr(&output)
        );
    }
    let file = dir.file("ok.json", &usage_evaluation("EVAL-PRICED", 1));
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
    assert!(plan.status.success(), "{}", stderr(&plan));
    assert!(
        stdout(&plan).contains("pricing     model-a: USD 3 input / 15 output per million tokens"),
        "{}",
        stdout(&plan)
    );
    assert!(stdout(&plan).contains("cybercore-usage.json (recorded as self-reported)"));
}

#[test]
fn suites_run_compare_and_expand() {
    let dir = TempDir::new("suite");
    let store = dir.0.join("store");
    let suite = dir.file("suite.json", &load_example("report-suite.json"));
    let plan = workbench(
        &["run", suite.to_str().unwrap(), "--dry-run"],
        &[("WORKBENCH_STORE", &store)],
    );
    assert!(plan.status.success(), "{}", stderr(&plan));
    assert!(stdout(&plan).contains(
        "suite REPORTS: 2 agent(s) × 6 task(s) × 2 repeat(s) → REPORTS-CAREFUL, REPORTS-SLOPPY"
    ));
    assert!(!store.exists(), "a dry run writes nothing");

    let output = workbench(
        &["run", suite.to_str().unwrap()],
        &[("WORKBENCH_STORE", &store)],
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
    let text = stdout(&output);
    assert!(
        text.contains("froze analysis plan PLAN-REPORTS-CAREFUL"),
        "{text}"
    );
    assert!(text.contains("compare with: workbench compare REPORTS-SLOPPY REPORTS-CAREFUL"));

    let compare = workbench(
        &["compare", "REPORTS-SLOPPY", "REPORTS-CAREFUL", "--json"],
        &[("WORKBENCH_STORE", &store)],
    );
    assert!(compare.status.success(), "{}", stderr(&compare));
    let out: Value = serde_json::from_str(&stdout(&compare)).unwrap();
    assert_eq!(out["baseline"]["passes"], 2);
    assert_eq!(out["treatment"]["passes"], 12);
    assert_eq!(out["comparison"]["status"], "comparable");
    assert_eq!(out["comparison"]["matched_tasks"], 6);
    assert_eq!(out["comparison"]["inferential_status"], "confirmatory");
    assert_eq!(
        out["comparison"]["declared_comparison_id"],
        "CAREFUL-vs-SLOPPY"
    );
    assert_eq!(out["speed_and_cost"][2]["status"], "comparable");

    // Running the suite again resumes both evaluations and runs nothing.
    let again = workbench(
        &["run", suite.to_str().unwrap()],
        &[("WORKBENCH_STORE", &store)],
    );
    assert!(again.status.success(), "{}", stderr(&again));
    assert_eq!(stdout(&again).matches("executed 0 attempt(s)").count(), 2);

    // Expanded files are the same evaluations: running one resumes it.
    let out_dir = dir.0.join("expanded");
    let expand = workbench(
        &[
            "expand",
            suite.to_str().unwrap(),
            "--out",
            out_dir.to_str().unwrap(),
        ],
        &[],
    );
    assert!(expand.status.success(), "{}", stderr(&expand));
    let expanded = out_dir.join("REPORTS-SLOPPY.json");
    let resumed = workbench(
        &["run", expanded.to_str().unwrap()],
        &[("WORKBENCH_STORE", &store)],
    );
    assert!(
        stdout(&resumed).contains("resuming REPORTS-SLOPPY"),
        "{}",
        stdout(&resumed)
    );
    assert!(stdout(&resumed).contains("executed 0 attempt(s)"));
    // Expanding again leaves identical files alone and refuses to overwrite others.
    let unchanged = workbench(
        &[
            "expand",
            suite.to_str().unwrap(),
            "--out",
            out_dir.to_str().unwrap(),
        ],
        &[],
    );
    assert!(stdout(&unchanged).contains("unchanged"));
    fs::write(&expanded, "{}").unwrap();
    let refused = workbench(
        &[
            "expand",
            suite.to_str().unwrap(),
            "--out",
            out_dir.to_str().unwrap(),
        ],
        &[],
    );
    assert_eq!(refused.status.code(), Some(1));
    assert!(stderr(&refused).contains("already exists with different content"));
}

#[test]
fn suite_specs_are_validated() {
    let dir = TempDir::new("suite-spec");
    let cases: Vec<BadFileCase> = vec![
        (
            "repeats",
            Box::new(|v| v["repeats"] = 0.into()),
            "repeats must be between 1 and 100",
        ),
        (
            "agent-dup",
            Box::new(|v| {
                let first = v["agents"][0].clone();
                v["agents"].as_array_mut().unwrap().push(first)
            }),
            "agents[CAREFUL] is listed more than once",
        ),
        (
            "task-id",
            Box::new(|v| v["tasks"][0]["task_id"] = "bad id".into()),
            "tasks[].task_id must be 1 to 40",
        ),
        (
            "no-criteria",
            Box::new(|v| v["tasks"][0]["checks"] = serde_json::json!([])),
            "tasks[ONES] needs at least one entry in checks or judge",
        ),
        (
            "judge-consent",
            Box::new(|v| {
                v["tasks"][0]["judge"] = serde_json::json!([{"criterion_id": "j", "rubric": "r"}])
            }),
            "tasks[ONES].judge needs the suite's judge settings",
        ),
        (
            "env-clash",
            Box::new(|v| v["agents"][0]["environment"] = serde_json::json!({"TASK_PROMPT": "x"})),
            "tasks[ONES].environment.TASK_PROMPT is also set by agents[CAREFUL]",
        ),
        (
            "compare-unknown",
            Box::new(|v| v["compare"][0]["baseline"] = "NOBODY".into()),
            "compare[0].baseline \"NOBODY\" is not an agent_id",
        ),
        (
            "credentials",
            Box::new(|v| v["agents"][0]["credentials"] = serde_json::json!(["KEY"])),
            "agents[CAREFUL].credentials need agents[CAREFUL].network",
        ),
        (
            "unknown-field",
            Box::new(|v| v["tasks"][0]["expected"] = "6".into()),
            "unknown field `expected`",
        ),
        // Errors found in the expansion name the expanded evaluation.
        (
            "bad-check",
            Box::new(|v| v["tasks"][0]["checks"][0]["kind"] = "file_matches".into()),
            "(expanded REPORTS-CAREFUL)",
        ),
        (
            "executable",
            Box::new(|v| v["agents"][1]["executable_id"] = "python".into()),
            "(expanded REPORTS-SLOPPY)",
        ),
    ];
    for (name, mutate, expected) in cases {
        let mut value = load_example("report-suite.json");
        mutate(&mut value);
        let file = dir.file(&format!("{name}.json"), &value);
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
        assert!(
            stderr(&output).contains(expected),
            "{name}: expected {expected:?} in {}",
            stderr(&output)
        );
    }
}

const REFERENCE_IMAGE: &str = "localhost/cybercore-reference-agent:0.1";

/// The fake Messages API (examples/reference-agent/fake_api.py) on loopback,
/// with a throwaway certificate for `localhost`. Stopped on drop.
struct FakeApi {
    child: std::process::Child,
    port: u16,
    cert: String,
}

impl FakeApi {
    fn start(dir: &Path, key: &str) -> Option<Self> {
        let (cert, private) = (dir.join("fake-cert.pem"), dir.join("fake-key.pem"));
        let made = Command::new("openssl")
            .args([
                "req", "-x509", "-newkey", "rsa:2048", "-nodes", "-days", "1",
            ])
            .args([
                "-subj",
                "/CN=localhost",
                "-addext",
                "subjectAltName=DNS:localhost",
            ])
            .arg("-keyout")
            .arg(&private)
            .arg("-out")
            .arg(&cert)
            .output()
            .ok()?;
        if !made.status.success() {
            return None;
        }
        let script = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../examples/reference-agent/fake_api.py");
        let mut child = Command::new("python3")
            .arg(script)
            .arg("--cert")
            .arg(&cert)
            .arg("--key")
            .arg(&private)
            .args(["--expect-key", key])
            .stdout(std::process::Stdio::piped())
            .spawn()
            .ok()?;
        let mut line = String::new();
        use std::io::BufRead;
        std::io::BufReader::new(child.stdout.take()?)
            .read_line(&mut line)
            .ok()?;
        let port = line.trim().strip_prefix("listening on ")?.parse().ok()?;
        Some(Self {
            child,
            port,
            cert: fs::read_to_string(&cert).ok()?,
        })
    }
}

impl Drop for FakeApi {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[test]
fn reference_agent_runs_through_egress_against_the_fake_api() {
    const KEY: &str = "wb-reference-agent-test-key-0123456789";
    let required = std::env::var_os("WORKBENCH_REQUIRE_REFERENCE_AGENT").is_some();
    let image = Command::new("podman")
        .args(["image", "exists", REFERENCE_IMAGE])
        .status()
        .is_ok_and(|s| s.success());
    if !image {
        assert!(
            !required,
            "WORKBENCH_REQUIRE_REFERENCE_AGENT is set but {REFERENCE_IMAGE} is not built"
        );
        eprintln!("SKIPPED: build {REFERENCE_IMAGE} first (see examples/reference-agent)");
        return;
    }
    let dir = TempDir::new("reference-agent");
    let fake =
        FakeApi::start(&dir.0, KEY).expect("openssl and python3 are needed for the fake API");

    let mut suite = load_example("reference-agent-suite.json");
    let egress = &mut suite["runner"]["egress"];
    egress["ports"] = serde_json::json!([fake.port]);
    egress["allow_private_destinations"] = true.into();
    egress["forwarder_path"] = static_forwarder().to_str().unwrap().into();
    egress["credentials"]["ANTHROPIC_API_KEY"]["from_env"] = "WB_REFERENCE_TEST_KEY".into();
    for agent in suite["agents"].as_array_mut().unwrap() {
        agent["network"]["hosts"] = serde_json::json!(["localhost"]);
        agent["environment"]["ANTHROPIC_BASE_URL"] =
            format!("https://localhost:{}", fake.port).into();
        agent["environment"]["AGENT_CA_FILE"] = "/workspace/input/fake-ca.pem".into();
    }
    for task in suite["tasks"].as_array_mut().unwrap() {
        task["files"]
            .as_array_mut()
            .unwrap()
            .push(serde_json::json!({"path": "fake-ca.pem", "content": fake.cert}));
    }
    let file = dir.file("suite.json", &suite);
    let store = dir.0.join("store");
    let output = Command::new(BIN)
        .args([
            "run",
            file.to_str().unwrap(),
            "--store",
            store.to_str().unwrap(),
        ])
        .env_remove("WORKBENCH_STORE")
        .env("WB_REFERENCE_TEST_KEY", KEY)
        .output()
        .unwrap();
    if !output.status.success() && stderr(&output).contains("runner:") {
        assert!(
            std::env::var_os("WORKBENCH_REQUIRE_PODMAN").is_none() && !required,
            "the container runner is unavailable: {}",
            stderr(&output).trim()
        );
        eprintln!(
            "SKIPPED: container runner unavailable here: {}",
            stderr(&output).trim()
        );
        return;
    }
    assert!(output.status.success(), "{}", stderr(&output));

    let compare = workbench(
        &["compare", "REFAGENT-HAIKU", "REFAGENT-OPUS", "--json"],
        &[("WORKBENCH_STORE", &store)],
    );
    assert!(compare.status.success(), "{}", stderr(&compare));
    let out: Value = serde_json::from_str(&stdout(&compare)).unwrap();
    // The fake's "haiku" writes the line count, which equals the total only
    // for ONES (three 1s): 1 task × 2 repeats.
    assert_eq!(out["treatment"]["passes"], 12, "{out:#}");
    assert_eq!(out["baseline"]["passes"], 2, "{out:#}");
    assert_eq!(out["comparison"]["inferential_status"], "confirmatory");
    // Four API calls per trial at fixed usage: 1 100 tokens a call for haiku,
    // 2 200 for opus; costs from the suite's declared prices.
    let tokens = &out["speed_and_cost"][1];
    assert_eq!(tokens["difference"].as_f64(), Some(4400.0), "{tokens}");
    let summary = workbench(
        &["summary", "REFAGENT-OPUS", "--json"],
        &[("WORKBENCH_STORE", &store)],
    );
    let opus: Value = serde_json::from_str(&stdout(&summary)).unwrap();
    assert_eq!(opus["speed_and_cost"][1]["estimate"].as_f64(), Some(8800.0));
    let cost = opus["speed_and_cost"][2]["estimate"].as_f64().unwrap();
    assert!((cost - 48_000.0).abs() < 1e-6, "{cost}"); // 8000 × $4 + 800 × $20 per million

    // The granted key never reaches the store.
    let mut stack = vec![store];
    while let Some(path) = stack.pop() {
        for entry in fs::read_dir(&path).unwrap().flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else {
                let bytes = fs::read(&path).unwrap();
                assert!(
                    !bytes.windows(KEY.len()).any(|w| w == KEY.as_bytes()),
                    "key leaked into {}",
                    path.display()
                );
            }
        }
    }
}
