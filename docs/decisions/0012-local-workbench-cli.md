# ADR-0012: Local `workbench` CLI

- **Status:** Accepted (implemented in `crates/workbench-cli`)
- **Date:** 2026-11-18
- **Builds on:** ADR-0009, ADR-0010, ADR-0011

## Context

Running an evaluation required writing Rust against `evaluation-store`. The README's design rule says not to expose API, CLI or MCP operations until the underlying application services, persistence, authorization behaviour and security boundaries are implemented and tested.

## Decision

- **A local-only binary, `workbench`,** with two subcommands:
  - `run <file> [--dry-run]` runs or resumes an evaluation;
  - `status <evaluation-id> [--json]` is read-only: no lease, no journal writes.
- **How it meets the design rule.** It calls only services that are implemented and tested: the store, durable scheduler, run records and the Bubblewrap runner. It has no network listener, no remote store and no credentials. Locally, the only principal is the operating-system user who owns the store, so there's no authorization surface to get wrong. The local runner's documented limits still apply: it's prototype isolation, unredacted output, and no secrets in task inputs. The CLI does not widen them.
- **Evaluation files** (`cybercore-workbench-evaluation/v0`) hold the frozen `EvaluationSchedule`, a `local_bubblewrap` runner spec, a `stdout_equals` verifier (hidden grader data, kept outside every `task_input`), the `project_ref`, and `store_output_evidence`. Unknown fields are rejected. Errors name the file and the field. Cross-checks:
  - every trial's executable is allowlisted;
  - verifier expectations name real trials;
  - paths are absolute;
  - derived record IDs fit the 128-character limit.
- **Store location:** `--store`, else `$WORKBENCH_STORE`, else `$XDG_DATA_HOME/cybercore-workbench` (by default `~/.local/share/cybercore-workbench`). There's no config file yet.
- **Dry run.** `run --dry-run` validates everything, including whether the store already holds a *different* schedule under the same ID, and prints the plan. It creates nothing, not even the store directory.
- **Identities** are `RUN-/REQ-/IDEMP-<evaluation>-<trial>-A<n>`. They're deterministic, so a resumed run reuses nothing and collides with nothing, as long as evaluation IDs are unique.
- **Lease.** The owner is `workbench-cli@<hostname>`, so a restarted run on the same host takes its own lease back immediately. The TTL is at least 10 minutes and at least twice the longest trial budget plus a minute, renewed after every attempt.
- **Exit codes:** 0 success (failed or inconclusive trial outcomes are results, not errors), 1 error, 2 usage error. A closed stdout (for example `| head`) ends the process quietly instead of panicking.

## Consequences

- An evaluation can be run, resumed after a crash, and inspected with no code.
- Only one runner type and one verifier type exist so far; both are tagged enums in the file format, so more can be added without breaking v0 files.
- The real-run CLI test skips with a printed notice on hosts where Bubblewrap cannot create user namespaces (some CI images); all other CLI tests run everywhere.
