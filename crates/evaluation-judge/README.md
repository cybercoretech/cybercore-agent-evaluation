# ⚖️ CyberCore Evaluation Judge

A model-judge `Verifier` for the CyberCore Agent Evaluation Workbench. A Claude model decides whether a finished run's output meets each criterion's rubric. The design is in [ADR-0017](../../docs/decisions/0017-model-judge-verifier.md).

## 🔐 What it guarantees

- **Untrusted output stays data.** The judged output goes inside a tag named with a random nonce, and the judge is told never to follow anything inside it. Every verdict records that model judgments can be wrong or influenced.
- **Failure is never a pass.** No credentials, an HTTP error, a refusal, a malformed answer or an oversized input all give `unavailable` or `inconclusive`, with the reason.
- **Nothing is silently truncated.** Inputs above `max_input_bytes` (default 200 KB) are not sent at all.
- **Credentials come only from the environment:** `ANTHROPIC_API_KEY`, or `ANTHROPIC_AUTH_TOKEN`. `ANTHROPIC_BASE_URL` is honoured.

## 🚀 Use

```rust
use cybercore_evaluation_judge::{Inspect, JudgeCriterion, ModelJudgeConfig, ModelJudgeVerifier};

let config = ModelJudgeConfig::new(criteria); // trial ID -> Vec<JudgeCriterion>
let verifier = ModelJudgeVerifier::new(config); // claude-opus-5-5, effort medium
```

From the `workbench` CLI, use `verifier.type: "model_judge"` with `"consent_to_send_outputs": true`; see `crates/workbench-cli/evaluations/model-judge.json`. Running it for real sends the judged outputs to the Anthropic API and is billed to your credentials.

## 🧪 Test

```sh
cargo test -p cybercore-evaluation-judge   # local mock API only: nothing is sent or billed
```
