use serde_json::Value;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_workbench");

fn example() -> Value {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("evaluations/cat-smoke.json");
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
