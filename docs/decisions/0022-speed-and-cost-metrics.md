# ADR-0022: Speed and Cost Metrics

- **Status:** Accepted (implemented in the v0 `run` contract, `evaluation-store::usage`, `evaluation-store::analysis` and `workbench-cli`)
- **Date:** 2026-11-19
- **Builds on:** ADR-0020 (cross-run summaries), ADR-0014 (output collection), ADR-0019 (egress)

## Context

Summaries reported only the pass rate. A faster, cheaper agent with the same pass rate looked identical, and so did a better agent that costs three times as much. Runners already measured each attempt's wall time, but nothing summarised it. The run record had fields for tokens and cost, but they stayed empty: the agent's model-API traffic is TLS-encrypted, and the egress proxy (ADR-0019) deliberately never decrypts it.

## Decision

- **Token source: the agent reports, and the record says so.** The owner chose this over two alternatives:
  - time only, which would have left cost out entirely;
  - a decrypting metering proxy, which would break ADR-0019's "never decrypt" design, need a CA certificate inside the container, and be specific to each API.
- **Usage file.** An agent may write `/workspace/output/cybercore-usage.json` (requires `collect_outputs`):
  - `format: "cybercore-usage/v0"`, `models` (1–32 entries of `model_id`, `input_tokens`, `output_tokens`) and an optional `tool_calls`;
  - unknown fields, duplicate models, counts above 10^12 and files over 64 KiB are invalid.
- **Contract (additive, v0).** The recorder writes the file's content to `run.agent_report.reported_usage`:
  - `source: agent_self_reported` and `report_status` (`reported` or `invalid`);
  - totals and per-model counts;
  - a cost, when priced.

  `resource_usage` keeps only what the workbench measured (wall time). An invalid report keeps only a reason the workbench wrote; agent-written text is never copied into the record. A valid fixture uses it, and an invalid fixture (a cost without a currency) is rejected by the Python, TypeScript and Rust suites.
- **Prices.** The evaluation file's `pricing` declares a currency and, per model ID, prices per million input and output tokens. The rules:
  - **Units.** Tokens times the price per million tokens is already in millionths of the currency, so the cost is stored as `cost_microunits`, with the currency and a `pricing_digest` of the canonical price table.
  - **Unpriced models.** If a report names a model with no declared price, it gets no cost and lists `unpriced_model_ids`.
  - **Validation.** Prices must be finite and between 0 and 10 000, the currency must be three capital letters, and `pricing` requires a trial that collects outputs.
- **Metrics (revision 1 each)**, all `lower_is_better`:
  - `workbench-wall-time`: the median and p90;
  - `workbench-reported-tokens`: the mean of input plus output tokens, plus the total;
  - `workbench-reported-cost`: the mean cost, plus the total.
- **Counting rules:**
  - **Unit and eligibility.** The unit is the planned trial, valued by its latest attempt. Trials with an outcome (pass, fail or no verdict) are eligible, since failed work also takes time and tokens. Invalid runs are excluded and unrun trials are pending, as for the pass rate.
  - **Missing values are never filled in.** An eligible trial without a value counts as missing (`missing_trial_count`), and any missing or pending trial makes the result `descriptive_only`.
  - **Price tables.** Costs from different price tables are never pooled (`insufficient_data`).
  - **Notes.** Results always state that tokens are self-reported and that cost is an estimate, not a bill.
- **Uncertainty:**
  - a 95% percentile bootstrap of the estimator that resamples tasks, from 5 distinct tasks up;
  - `computed` needs at least 10 observed trials, nothing missing and an interval;
  - seeds derive from the data.
- **Comparisons:**
  - **Pairing.** Tasks are paired, and the effect is the mean over matched tasks of the per-task mean difference (treatment − baseline), with a paired bootstrap interval and the relative change.
  - **Direction.** It's claimed only when the interval excludes 0.
  - **Not comparable.** Different task populations, or different price tables for cost, are `not_comparable` (`prohibited`).
  - **Always exploratory.** ADR-0021 declarations cover only the pass rate.
- **Records.** Each summary writes the three metric definitions (once) and three `metric_result`s (`MR-<tag>:wall-time`, `:tokens`, `:cost`). The `analysis_result` references all four results, and the pass rate stays the primary metric. `compare` writes one `comparison_result` per metric. `ANALYSIS_VERSION` is now `workbench-analysis/2`, because the analysis result changed shape.
- **CLI output:**
  - `summary` prints the time, token and cost lines;
  - `compare` prints a "speed and cost" block;
  - `--json` adds `speed_and_cost`;
  - the dry run shows the usage file's path and the declared prices.
- **Fixes made along the way:**
  - **Doubled attempt counts.** Summaries counted each run twice, because a run refers to its evaluation from more than one field. Each run is now counted once, so `attempts` and `run_attempt_count` are correct.
  - **Pending trials in pass-rate records.** `eligible_trial_count` now leaves out pending trials, so eligible plus excluded plus pending equals planned.

## Consequences

- An evaluation now shows the trade-off between quality, speed and cost, for example "same pass rate, 60% faster, 67% cheaper", with each number's provenance next to it.
- **Self-reported numbers can be wrong or gamed.** An agent can under-report, so token and cost claims are only as trustworthy as the agent's harness. Use them to compare well-behaved harnesses, not to audit adversarial ones.
- **Not covered:**
  - verified token metering;
  - CPU time and memory peaks;
  - retried attempts' usage (only the latest attempt counts);
  - predeclared speed and cost comparisons;
  - the pass-rate record's `denominator` still counts trials without a verdict, while `observed_trial_count` doesn't. The domain invariants expect these to match; this is left for a separate change.
