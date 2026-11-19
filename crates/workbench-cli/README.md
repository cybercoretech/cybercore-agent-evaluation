# 🧰 workbench

Run and inspect local CyberCore agent evaluations from the command line. The design is in [ADR-0012](../../docs/decisions/0012-local-workbench-cli.md).

## 🚀 Quick start

```sh
cargo build --release -p cybercore-workbench-cli

# See the plan without running or writing anything
./target/release/workbench run crates/workbench-cli/evaluations/cat-smoke.json --dry-run

# Run it (safe to repeat: finished trials are not re-run)
./target/release/workbench run crates/workbench-cli/evaluations/cat-smoke.json

# Inspect it
./target/release/workbench status EVAL-CAT-SMOKE
./target/release/workbench status EVAL-CAT-SMOKE --json
```

```
EVAL-CAT-SMOKE
  TRIAL           RUN                                   ATTEMPT  EXECUTION  VERIFICATION   OUTCOME
  TRIAL-GREETING  RUN-EVAL-CAT-SMOKE-TRIAL-GREETING-A1  1        finished   verified_pass  pass
  TRIAL-NUMBERS   RUN-EVAL-CAT-SMOKE-TRIAL-NUMBERS-A1   1        finished   verified_fail  fail

  latest attempt per trial: 1 fail, 1 pass
```

TRIAL-NUMBERS fails on purpose: its expected output says `4` where the file contains `3`.

## 📊 Summaries and comparisons

`workbench summary EVAL` gives an evaluation's pass rate with a 95% interval, and `workbench compare BASELINE TREATMENT` compares two evaluations run on the same tasks, for example two agents or two versions of one ([ADR-0020](../../docs/decisions/0020-cross-run-summaries.md)). Both accept `--json`.

```
EVAL-BASE (baseline) vs EVAL-TREAT (treatment)
  baseline    0.333 (4 of 12)
  treatment   0.917 (11 of 12)
  difference  +0.583   95% interval +0.333 to +0.833   (paired over 12 task(s))
  McNemar     p = 0.0156
  direction   favors treatment
  status      comparable
```

- **The pass rate:** each planned trial counts once, by its latest attempt. Trials **without a verdict** (inconclusive or not scored) **count as not passed**, and the summary says how many there were. Invalidated runs are excluded. Trials not run yet make the result provisional.
- **The interval:** Wilson's when each task ran once. When a task ran more than once, it's a bootstrap over tasks, so repeats don't count as independent evidence. With fewer than 5 tasks no bootstrap interval is given, and with fewer than 10 trials the result is "descriptive only".
- **Comparisons are paired by task.** They give the difference, a bootstrap interval and (for one trial per task) McNemar's exact test, and they're labelled exploratory. Evaluations over different task revisions are reported as **not comparable** rather than compared.
- **Records:** each command records its result as `metric_result`, `analysis_result` or `comparison_result` records. Summarising unchanged results again writes nothing new.

## 📄 Evaluation files

`evaluations/cat-smoke.json` is a complete example. Its top-level fields:

| Field | Meaning |
|---|---|
| `format` | `cybercore-workbench-evaluation/v0` |
| `schedule` | the frozen `EvaluationSchedule`: trials, budgets, deny-all policies, retry policy, and `max_active_runs` (how many attempts run at once). It's immutable once stored. |
| `runner` | `{"type": "local_bubblewrap", ...}` (prototype isolation), or `{"type": "podman", "images": {"ubuntu": "docker.io/library/ubuntu:22.04"}, "allowed_executables": {"sh": "/bin/sh"}, "limits": {...}, "resources": {"memory_bytes": ..., "cpu_millis": ..., "pids_limit": ...}}` (rootless containers with real limits). Every trial's `task_input.executable_id` must be listed, and with several images each trial needs an `image_id`. |
| `verifier` | `{"type": "stdout_equals", "expected": {...}}`, or `{"type": "checks", "checks": {"TRIAL-ID": [{"criterion_id": "report-written", "kind": "file_equals", "path": "out/report.txt", "expected": "..."}]}}` with kinds `stdout_equals`, `stdout_contains`, `file_exists`, `file_absent`, `file_equals` and `file_contains`. Or `{"type": "model_judge", "consent_to_send_outputs": true, "criteria": {"TRIAL-ID": [{"criterion_id": "...", "rubric": "...", "file": "optional"}]}}`, where a Claude model judges each rubric ([ADR-0017](../../docs/decisions/0017-model-judge-verifier.md)): judged outputs are sent to the Anthropic API using `ANTHROPIC_API_KEY`. Or `{"type": "composite", "verifiers": [ ...checks..., ...model_judge... ]}` to combine exact checks and judged criteria in one evaluation ([ADR-0018](../../docs/decisions/0018-composite-verifier.md)); criterion IDs must be unique per trial across the verifiers. This is hidden grader data, never part of a `task_input`. |
| `project_ref` | the project the output evidence belongs to |
| `store_output_evidence` | `true` stores each runner output as evidence. Only use it for non-sensitive tasks: the local runner does not redact. |
| `task_revisions` | optional `task_revision` records stored before the run. Their criteria's `criticality` (mandatory, major or minor) and `required_criterion_ids` decide each run's outcome ([ADR-0013](../../docs/decisions/0013-concurrency-and-outcome-rules.md)). |

Unknown fields are rejected, and every error names the file and the field.

## 📦 Container runner

`runner.type: "podman"` runs every attempt in its own rootless Podman container ([ADR-0015](../../docs/decisions/0015-podman-container-runner.md)): no network, all capabilities dropped, a read-only image, size-limited tmpfs mounts, and cgroup memory, CPU and process limits. The runner checks that the limits really apply and refuses to run otherwise. Images must already be pulled, because the runner runs them by image ID and never pulls. A task stopped by the memory limit is recorded as `errored` / `not_scored`. See `evaluations/container-report.json`.

**Allowlisted network and credentials ([ADR-0019](../../docs/decisions/0019-egress-allowlist.md)).** A Podman runner with an `egress` block lets a trial whose `network_policy` is `{"mode": "allowlist", "hosts": [...]}` reach exactly those hosts:

```json
"egress": {
  "ports": [443],
  "max_connections": 64,
  "max_bytes": 67108864,
  "credentials": {"ANTHROPIC_API_KEY": {"from_env": "ANTHROPIC_API_KEY"}}
}
```

- **How it works:** the container still has no network. Its only way out is a host proxy, reached through a mounted Unix socket. The proxy accepts only HTTPS `CONNECT` to an allowlisted name on an allowed port, checks that the TLS server name matches, refuses IP addresses and private destinations, and logs every attempt in the run's output (`network.egress`). `HTTPS_PROXY` is set inside the container, so normal HTTP clients just work.
- **Credentials:** a trial lists the ones it needs in `task_input.credentials`. Their values are read from the host environment when the run starts and never appear in the file or on a command line. Granted values are replaced with `[REDACTED:NAME]` in stdout and stderr, and an output file containing one is rejected.
- **The forwarder:** egress needs a statically linked forwarder, by default next to the `workbench` binary. Build it with `RUSTFLAGS='-C target-feature=+crt-static' cargo build --release -p cybercore-egress-forwarder --target x86_64-unknown-linux-gnu`, or set `egress.forwarder_path`.
- **Example:** `evaluations/container-egress.json` reaches `api.anthropic.com` (a TLS handshake only, never an API request) and shows `example.com` refused and direct connections blocked.

## 📁 Checking files the agent writes

Set `"collect_outputs": true` in a trial's `task_input` and give it a positive `budget.max_artifact_bytes`. The sandbox then gets a writable `/workspace/output`; regular files left there after a clean exit are collected, digested and handed to `checks`. Symlinks, FIFOs and devices are rejected unread, and runs that time out have no collected files. `evaluations/report-files.json` is a complete example: one trial writes the report correctly, the other writes it to the wrong path and leaves a scratch file. See [ADR-0014](../../docs/decisions/0014-workspace-output-files.md).

## 🗄️ Store and environment

| Setting | Effect |
|---|---|
| `--store DIR` | store directory (wins over everything) |
| `WORKBENCH_STORE` | store directory when `--store` is absent |
| `XDG_DATA_HOME` | default store is `$XDG_DATA_HOME/cybercore-workbench` (else `~/.local/share/cybercore-workbench`) |

The store holds the records, the scheduler journal and the evidence; see [evaluation-store](../evaluation-store/README.md).

## 🔢 Exit codes

| Code | Meaning |
|---|---|
| 0 | success; failed or inconclusive trial outcomes are results, not errors |
| 1 | error (bad file, schedule conflict, runner unavailable, lease held by another host…) |
| 2 | usage error |

## ⚠️ Limits

The local Bubblewrap runner is prototype isolation, with no cgroup quotas and no output redaction. Don't run hostile code or pass secrets in task inputs. See [LOCAL-PROCESS-RUNNER.md](../../docs/architecture/LOCAL-PROCESS-RUNNER.md).
