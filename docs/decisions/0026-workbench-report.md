# ADR-0026: `workbench report`

- **Status:** Accepted (implemented in `workbench-cli`: `report.rs`, `workbench report`)
- **Date:** 2026-11-19
- **Builds on:** ADR-0020, ADR-0021, ADR-0022 (summaries, comparisons, speed and cost), ADR-0024 (suites)

## Context

Results lived in terminal output and JSON. Sharing an evaluation meant copying several command outputs, and the caveats that make the numbers honest (self-reported tokens, exploratory versus confirmatory, provisional results) were easy to lose along the way.

## Decision

- **Command.** `workbench report EVAL [EVAL...] [--format markdown|html]` prints one page to stdout. Markdown is the default. The first evaluation is the baseline, and each of the others is compared with it.
- **Same numbers.** The report calls the same functions as `summary` and `compare`, and records the same results. Record IDs derive from content, so repeating it writes nothing new.
- **One document model, two renderers.** The page is a list of blocks (headings, paragraphs, lists, tables), rendered to Markdown and to HTML, so both formats always say the same thing. The page holds:
  - **Header:** generated time (UTC, from the standard library) and workbench version, plus the evaluation, task and trial counts and the baseline.
  - **Results:** pass rate (passes of the denominator, with its 95% interval), status, median wall time, and tokens and cost per trial.
  - **Each comparison:** the pass-rate difference and interval with the winner named, the McNemar p-value when present, the inference (naming the declaration when confirmatory), practical significance when assessed, then wall time, tokens and cost with their relative change and direction.
  - **By task:** each task's key and family from the stored task revisions, and passes out of trials with an outcome for every evaluation, with ✗ on any failure.
  - **Caveats:** every limitation from every summary and comparison, deduplicated in order.
  - **Provenance:** each evaluation's metric and analysis result IDs and its frozen plan digest, plus the comparison record IDs for each metric.
- **Safe output:**
  - **No overwrites.** It writes to stdout only, so it never overwrites a file the user didn't name.
  - **Escaping.** Markdown table cells escape `|` and backslashes.
  - **Self-contained HTML.** All text is escaped, with no scripts, external resources or links. The styles use colour tokens with a dark-mode override and themed scrollbars, and wide tables scroll inside their card.

## Consequences

- One command produces something to paste into a PR or send to someone, with the caveats attached to the numbers.
- **Not covered:**
  - charts;
  - per-family breakdowns beyond the by-task table;
  - a `--json` form (`summary` and `compare` already have one);
  - choosing a baseline other than the first evaluation.
