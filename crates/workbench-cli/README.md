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

## 📄 Evaluation files

`evaluations/cat-smoke.json` is a complete example. Its top-level fields:

| Field | Meaning |
|---|---|
| `format` | `cybercore-workbench-evaluation/v0` |
| `schedule` | the frozen `EvaluationSchedule`: trials, budgets, deny-all policies, retry policy. It's immutable once stored. |
| `runner` | `{"type": "local_bubblewrap", "allowed_executables": {...}, "limits": {...}}`; every trial's `task_input.executable_id` must be listed |
| `verifier` | `{"type": "stdout_equals", "expected": {"TRIAL-ID": "exact stdout"}}`. This is hidden grader data, never part of a `task_input`. |
| `project_ref` | the project the output evidence belongs to |
| `store_output_evidence` | `true` stores each runner output as evidence. Only use it for non-sensitive tasks: the local runner does not redact. |

Unknown fields are rejected, and every error names the file and the field.

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
