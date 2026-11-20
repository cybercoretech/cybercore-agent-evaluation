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

## 🧩 Task suites

Writing every trial out in full gets long. A **task suite** lists the tasks once and the agents once, and the workbench builds one evaluation per agent ([ADR-0024](../../docs/decisions/0024-task-suites.md)). `evaluations/report-suite.json` is a complete example: a careful and a sloppy shell "agent" on six report tasks, each run twice.

```sh
./target/release/workbench run crates/workbench-cli/evaluations/report-suite.json --dry-run
./target/release/workbench run crates/workbench-cli/evaluations/report-suite.json
./target/release/workbench compare REPORTS-SLOPPY REPORTS-CAREFUL

# Write the generated evaluation files to read (or run on their own)
./target/release/workbench expand crates/workbench-cli/evaluations/report-suite.json --out generated/
```

```json
{
  "format": "cybercore-workbench-suite/v0",
  "suite_id": "REPORTS",
  "repeats": 2,
  "runner": { "type": "local_bubblewrap", "allowed_executables": {"sh": "/usr/bin/sh"}, "limits": {} },
  "defaults": { "collect_outputs": true },
  "agents": [
    { "agent_id": "CAREFUL", "executable_id": "sh", "args": ["-c", "..."] },
    { "agent_id": "SLOPPY",  "executable_id": "sh", "args": ["-c", "..."] }
  ],
  "tasks": [
    { "task_id": "SMALL", "family": "arithmetic", "objective": "Write the sum to out/report.txt.",
      "files": [{"path": "numbers.txt", "content": "1\n2\n3\n"}],
      "environment": {"TASK_PROMPT": "Sum the numbers ..."},
      "checks": [{"criterion_id": "total-written", "kind": "file_equals", "path": "out/report.txt", "expected": "6\n"}] }
  ],
  "compare": [{ "baseline": "SLOPPY", "treatment": "CAREFUL", "minimum_matched_tasks": 5 }]
}
```

- **What it builds:** one evaluation per agent (`REPORTS-CAREFUL`, `REPORTS-SLOPPY`). Each runs every task `repeats` times, as trials `TRIAL-REPORTS-CAREFUL-SMALL-R1` and so on.
- **Each trial** is the agent's command, plus the task's input files (in `/workspace/input`) and environment variables. Environment names are added to the runner's allowed list automatically. A name can't be set by both an agent and a task.
- **Task records are generated** from each task (objective, family, difficulty, and criteria from its checks and rubrics), and every agent gets identical ones, so comparisons line up.
- **Verification:** tasks use the `checks` kinds. A task may add `judge` rubrics; with those, the checks are combined with the model judge, and the suite needs `judge: {"consent_to_send_outputs": true}`.
- **Network:** an agent with `network: {"hosts": [...]}` gets an allowlist, and with `credentials` it gets granted keys (Podman runner with `egress`).
- **Comparisons:** `compare` entries are frozen in advance in the treatment's plan, so they can be confirmatory.
- **Budgets:** each value comes from the task, then the suite's `defaults.budget`, then the runner's limits.
- **Optional fields:** `project_ref`, `store_output_evidence`, `retry_policy`, `pricing`, `decision_question` and `created_at` (for the generated task records).
- **Running:** every agent's evaluation is checked before the first one runs, and they then run in the listed order. Running the suite again resumes them.
- **Real agents:** `evaluations/reference-agent-suite.json` runs the [reference agent](../../examples/reference-agent/README.md), a small Claude SDK agent, through allowlisted egress. Its test runs it against a fake API for free.
- **`expand`** writes ordinary evaluation files. It never overwrites a file with different content. The IDs are fixed, so change `suite_id` when you change the tasks.

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

**Comparisons declared in advance ([ADR-0021](../../docs/decisions/0021-predeclared-comparisons.md)).** An evaluation file can declare the question it answers and the comparisons it will be judged by, before it runs:

```json
"analysis_plan": {
  "decision_question": "Does agent B pass more tasks than agent A?",
  "comparisons": [{
    "comparison_id": "b-vs-a",
    "baseline_evaluation_id": "EVAL-AGENT-A",
    "alpha": 0.05,
    "minimum_matched_tasks": 10,
    "practical_significance_threshold": 0.1
  }]
}
```

- **Freezing:** `workbench run` freezes the plan as the schedule's `plan_ref` before the first trial runs. It can't change afterwards; a changed plan is refused, so give it a new `plan_ref` and `evaluation_ref`.
- **When a comparison is confirmatory:** `workbench compare EVAL-AGENT-A EVAL-B` gives a **confirmatory** result only if three things hold:
  - the plan was frozen before the treatment had any result;
  - the comparison is comparable with an interval;
  - the declared minimum of matched tasks is met.

  Otherwise it stays exploratory and says why.
- **The rule:** it's the declared one, applied unchanged. McNemar's exact test (or, with repeated tasks, the paired bootstrap interval) at `alpha`, split Bonferroni-style across the plan's comparisons. With a threshold, the difference is also judged meaningful, not meaningful, or uncertain.

## 📝 Reports

`workbench report` turns one or more evaluations into a single page you can share ([ADR-0026](../../docs/decisions/0026-workbench-report.md)). The first evaluation is the baseline, and each of the others is compared with it:

```sh
workbench report REPORTS-SLOPPY REPORTS-CAREFUL > report.md
workbench report REPORTS-SLOPPY REPORTS-CAREFUL --format html > report.html
```

- **What's on the page:**
  - a results table: pass rate with its interval and status, median time, tokens and cost per trial;
  - each comparison, with its inference (for example "confirmatory (predeclared as CAREFUL-vs-SLOPPY)"), plus speed and cost;
  - a by-task table of passes out of trials, with failures marked ✗;
  - every caveat, said once;
  - the record IDs behind each number, and the frozen plan's digest.
- **Same numbers as the commands.** The report computes and records exactly what `summary` and `compare` do.
- **Output.** It prints to stdout, so it never overwrites a file you didn't name. The HTML is one self-contained file, with no scripts or outside links, and works in light and dark mode.

## ⏱️ Speed and cost

`workbench summary` also reports how long each trial took and, when the agent says so, how many tokens it used and what that cost ([ADR-0022](../../docs/decisions/0022-speed-and-cost-metrics.md)). `workbench compare` pairs them by task.

```
  wall time   median 108 ms   95% interval 107 ms – 113 ms   p90 138 ms   (12 of 12 trial(s); computed)
  tokens      mean 1805 tokens   95% interval 1585 tokens – 2016 tokens   total 21660 tokens   (12 of 12 trial(s), self-reported; computed)
  cost        mean 0.0085 USD   95% interval 0.0076 USD – 0.0093 USD   total 0.1017 USD   (12 of 12 trial(s), self-reported; computed)
```

- **Wall time is measured** by the runner, for every trial with an outcome (failed attempts take time too). It's summarised by its median and 90th percentile.
- **Tokens are self-reported.** The workbench can't see inside an agent's encrypted model-API traffic. Instead, an agent may write `/workspace/output/cybercore-usage.json` (the trial needs `task_input.collect_outputs: true`):

  ```json
  {"format": "cybercore-usage/v0",
   "models": [{"model_id": "claude-sonnet-5-5", "input_tokens": 12000, "output_tokens": 3400}],
   "tool_calls": 7}
  ```

  The run records it as `agent_report.reported_usage` with `source: agent_self_reported`, never as a measurement, and every summary says so. An invalid file is recorded as invalid with a reason the workbench writes (agent text is never copied).
- **Cost comes from prices you declare.** The evaluation file's `pricing` holds prices per million tokens for each model ID agents report:

  ```json
  "pricing": {"currency": "USD", "models": {
    "claude-sonnet-5-5": {"input_per_million_tokens": 3.0, "output_per_million_tokens": 15.0}}}
  ```

  Cost is recorded in millionths of the currency, together with a digest of the price table. A report naming an unpriced model gets no cost. Costs made under different price tables are never pooled or compared. It's an estimate, not a bill.
- **Missing values are counted, never filled in.** A trial without a measured time, without a usage report, or with an invalid or unpriced one counts as missing and keeps the result "descriptive only". Only each trial's latest attempt counts.
- **Comparisons** give the mean per-task difference (treatment − baseline), the relative change and a paired bootstrap interval. Lower is better. They're always exploratory; this revision can't declare them in advance.

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
| `pricing` | optional token prices per model, for costing agent-reported usage (see **Speed and cost** above). Requires at least one trial with `task_input.collect_outputs`. |

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
