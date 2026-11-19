# ADR-0017: Model-Judge Verifier

- **Status:** Accepted (implemented in `crates/evaluation-judge`, wired into `workbench-cli`)
- **Date:** 2026-11-19
- **Builds on:** ADR-0011 (Verifier port), ADR-0013 (outcome rules), ADR-0014 (output files)

## Context

Deterministic checks (ADR-0011, ADR-0014) cover exact outputs and files, but many acceptance criteria need judgment, such as "explains the root cause" or "the report is accurate and courteous". The skill's security guidance requires explicit consent before outputs go to an external model. A judged output is untrusted: it may try to persuade or instruct the judge.

## Decision

### Asynchronous verification in the loop

`Verifier` gains `verify_async`, a default method that calls `verify`, so existing verifiers are unchanged. `run_ready_trials` now verifies **inside each attempt's own future**. Previously verification ran synchronously between polls: a seconds-long model call would have stopped polling of the other in-flight attempts, letting their pipes fill and their wall-clock budgets run, which risks false timeouts. Crash catch-up still uses the synchronous `verify`, which is safe because nothing is in flight then. A test proves four 300 ms verifications overlap instead of serializing.

### `ModelJudgeVerifier` (`crates/evaluation-judge`)

**Transport.** Raw HTTP to `POST /v1/messages` with `anthropic-version: 2026-10-21`; there is no official Rust SDK. The blocking client (`ureq`) runs on Tokio's blocking pool from `verify_async`.

**Request:**
- the model defaults to `claude-opus-5-5`;
- thinking is left out (it's always on for that model), and `output_config.effort` defaults to `medium`;
- `output_config.format` is a JSON schema with `verdict` (`pass`, `fail` or `inconclusive`) and `rationale`;
- for models that accept it, the server-side `fallbacks: "default"` parameter is sent with the `server-side-fallback-2026-07-01` beta. A verdict served by a fallback model says so in its limitations and method.

**Prompt-injection hardening:**
- the judged output is placed inside a tag named with a 128-bit random nonce (`judged_output_<hex>`), so the output cannot close it;
- the system prompt tells the judge to treat that content strictly as data and never follow it;
- every verdict records the limitation that model judgments can be mistaken or influenced.

**Failure is never a pass:**
- missing credentials, transport errors and non-200 responses become `unavailable`, recording the error *type* only, never the message;
- a `refusal` or `max_tokens` stop, or an answer outside the schema, becomes `inconclusive`;
- inputs that are truncated, uncollected, non-UTF-8 or above `max_input_bytes` (default 200 KB) are **never sent**, and become `unavailable`; nothing is silently truncated.

**Credentials.** Only from `ANTHROPIC_API_KEY` (sent as `x-api-key`) or `ANTHROPIC_AUTH_TOKEN` (sent as `Authorization: Bearer` with the OAuth beta). Never from evaluation files or argv. `ANTHROPIC_BASE_URL` is honoured.

**Records.** `verifier_type: model_judge` and `verifier_id: model-judge:<model>`. The rationale is clipped to the schema's 4,096 characters. File criteria cite the file's evidence.

### CLI

`verifier.type: "model_judge"` with `consent_to_send_outputs`, which **must be `true`**, an optional `model`, `effort` and `max_input_bytes`, and per-trial `criteria` of `{criterion_id, rubric, file?}`. Rubrics are hidden grader data. `--dry-run` states that judged outputs go to the Anthropic API.

### Testing

All tests use a local mock of the Messages API, so nothing is sent or billed. They cover:
- the request shape, and isolation of a planted injection inside the nonce-named tag;
- HTTP 429, refusals and malformed answers;
- inputs that are never sent;
- labelling of fallback verdicts;
- the async path;
- a full `workbench run` in the Bubblewrap sandbox judged through the mock.

## Consequences

- Rubric-based criteria join deterministic ones under the same outcome rules (a mandatory criterion judged `fail` fails the run).
- Judgments cost money and can vary between runs. A judged `inconclusive` or `unavailable` never becomes a pass, so the conservative defaults hold.
- One verifier per evaluation for now: combining deterministic `checks` and judged criteria in one evaluation needs a composite verifier with per-verdict verifier identity (follow-up).
- No real API call is made in tests or CI. Running the example for real (`evaluations/model-judge.json`) sends the two trials' stdout to the Anthropic API and is billed to the caller's credentials.
