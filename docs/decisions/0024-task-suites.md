# ADR-0024: Task Suites

- **Status:** Accepted (implemented in `workbench-cli`: `suite.rs`, `workbench run` on a suite, `workbench expand`)
- **Date:** 2026-11-19
- **Builds on:** ADR-0012 (CLI), ADR-0020 and ADR-0021 (comparisons), ADR-0022 (usage and pricing)

## Context

An evaluation file spells out every trial in full: the command, input files, budget, policies and per-trial verifier entries. Comparing two agents on twelve tasks with three repeats meant writing 72 near-identical trial blocks, plus 72 verifier entries and matching task revisions. Every test and demo so far generated its evaluation files with a script. That was the main barrier to real use.

## Decision

- **Format.** A new input format, `cybercore-workbench-suite/v0`, holds `suite_id`, `repeats` (1–100), a shared `runner`, `defaults`, `agents`, `tasks`, and optionally `compare`, `pricing`, `judge`, `retry_policy`, `project_ref`, `store_output_evidence`, `decision_question` and `created_at`. **No contract or evaluation-file change.** A suite is pure input sugar.
- **Expansion is deterministic.** It gives one ordinary evaluation file per agent:
  - **IDs.** Evaluation `<suite>-<agent>`, plan `PLAN-<suite>-<agent>`, trials `TRIAL-<suite>-<agent>-<task>-R<n>`, task revisions `TASK-<suite>-<task>` and fingerprint `FP-<suite>-<agent>`, unless the agent sets one.
  - **Trials.** A trial is the agent's command, image, credentials and network allowlist, plus the task's input files and environment.
  - **Environment.** Agent and task environment names must not overlap, and they're added to the runner's `allowed_environment_keys`.
  - **Budgets.** Each value comes from the task, then `defaults.budget`, then the runner's limits; artifacts are budgeted only when outputs are collected.
  - **Task revisions** are generated from the task (objective, instruction, family, difficulty, and one mandatory criterion per check or rubric) and shared by every agent. So the task-population fingerprints match and comparisons are possible by construction. `created_at` defaults to a fixed date so re-expansion is byte-identical.
  - **Verifier.** It's `checks`. Tasks with `judge` rubrics combine it with `model_judge` into a `composite`, and that requires the suite's `judge.consent_to_send_outputs`.
  - **Comparisons.** `compare` entries become the treatment evaluation's `analysis_plan` (ADR-0021), so they're frozen before any treatment trial runs.
- **Validation in two layers:**
  - **The suite** checks its own fields: IDs, uniqueness, criteria present, consent, environment clashes, comparison references, credentials only with a network allowlist.
  - **Each expanded file** then goes through the same parser and checks as a hand-written file (`EvaluationFile::from_value`, the schedule check, task-revision contract validation). Its errors are labelled `(expanded <evaluation>)`.
- **Commands:**
  - **`workbench run SUITE`** checks every expanded evaluation before running any, then runs them in the listed order and prints the matching `workbench compare` commands. It's safe to repeat: each evaluation resumes. `--dry-run` shows every plan.
  - **`workbench expand SUITE --out DIR`** writes the evaluation files. An identical existing file is left alone, and a different one is never overwritten.
- **Example:** `evaluations/report-suite.json` has a careful and a sloppy shell agent on six tasks, run twice, with usage files, pricing and a declared comparison that comes out confirmatory.

## Consequences

- A two-agent comparison is now about one page of JSON, and the expanded files remain inspectable and runnable on their own. A test checks that a file from `expand` resumes the same evaluation the suite created.
- IDs are fixed by name, so editing a task under the same `suite_id` would conflict with the stored, immutable schedule. The store refuses, and the docs say to use a new `suite_id`.
- **Not covered:**
  - matrix dimensions beyond agents × tasks × repeats (such as models or prompts as separate axes);
  - interleaving agents to spread out time-of-day effects;
  - per-check criticality;
  - task files loaded from disk.
