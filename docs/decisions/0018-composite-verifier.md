# ADR-0018: Composite Verifier and Per-Verdict Verifier Identity

- **Status:** Accepted (implemented in `evaluation-store::composite`, wired into `workbench-cli`)
- **Date:** 2026-11-19
- **Builds on:** ADR-0011 (Verifier port), ADR-0014 (checks), ADR-0017 (model judge)

## Context

An evaluation could use only one verifier, so a task could not combine exact checks (files, stdout) with judged rubric criteria. The recorder also wrote one verifier identity on every `verification_result`, so a mix would have mislabelled who judged what.

## Decision

- **Attributed verdicts.** `Verifier` gains three default methods; the defaults keep every existing verifier unchanged:
  - `verify_attributed` and `verify_attributed_async` return each verdict together with the identity of the verifier that produced it. The defaults attribute everything to `identity()`.
  - `covers(trial_id)` defaults to `true`.

  The driver and recorder use the attributed forms, so each `verification_result` records its own verifier ID and type: `deterministic`, `model_judge`, or `hybrid` for composite-level verdicts.
- **`covers` overrides.** `StdoutEqualsVerifier`, `ChecksVerifier` and `ModelJudgeVerifier` return `false` for trials they have nothing configured for.
- **`CompositeVerifier`** runs each sub-verifier that covers the trial, in order, keeping attributions. Its async form awaits each sub-verifier's async form, so a judge inside a composite still never blocks the loop. It also handles two edge cases:
  - **A criterion ID produced by more than one sub-verifier for a trial** becomes a single `unavailable` verdict. Their records would collide, and silently preferring one would hide the other.
  - **A trial no sub-verifier covers** gets one `unavailable` verdict (`no-verifier-configured`), never a pass.
- **CLI.** `verifier.type: "composite"` with `verifiers: [...]`. It validates each sub-verifier with field paths such as `verifier.verifiers[1].consent_to_send_outputs`. It rejects nesting, an empty list, and duplicate `(trial, criterion)` pairs across sub-verifiers. `--dry-run` summarizes the parts. Example: `evaluations/composite-report.json`, with exact file checks plus a judged clarity criterion.
- **Judge error messages.** HTTP errors from the model judge now record the API's error message, clipped to 300 characters, next to the error type. The real smoke test showed why: the actionable cause, "credit balance is too low", was otherwise hidden. API messages describe the problem, not the request.

## Consequences

- Exact and judged criteria share one evaluation and one set of outcome rules (ADR-0013). A failed mandatory exact check fails the run whatever the judge says, and a judged criterion with nothing to read is `unavailable`, not a pass.
- Criterion IDs are the join key, so they must be unique per trial across verifiers. The CLI enforces that up front, and the composite enforces it again at run time.
