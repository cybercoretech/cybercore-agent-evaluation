# 🤖 Reference agent

A small, real, model-backed agent to evaluate with the workbench, and a template for wrapping your own ([ADR-0025](../../docs/decisions/0025-reference-agent.md)).

- **`agent.py`**: about 150 lines on the official Anthropic SDK (`anthropic` 2.5.0, using its tool runner).
  - **Input:** the task comes from `TASK_PROMPT` and the files in `/workspace/input`.
  - **Tools:** three file tools, confined to `/workspace/input` (read only) and `/workspace/output`.
  - **Usage report:** whatever happens, it writes `/workspace/output/cybercore-usage.json` from the API's own usage counts, so `workbench summary` can report tokens and cost.
- **`Containerfile`**: the image the workbench runs (the runner never pulls or builds).
- **`fake_api.py`**: a fake Claude Messages API (standard library only) that follows a fixed script. Tests and demos cost nothing.

## Build the image

```sh
podman build -t localhost/cybercore-reference-agent:0.1 examples/reference-agent
```

## Try it for free against the fake API

The automated test does exactly this, end to end through the egress proxy:

```sh
cargo test -p cybercore-workbench-cli --test cli -- reference_agent_ --nocapture
```

The fake writes the correct total for any model, except that a model ID containing `haiku` plays a weaker model and writes the line count. So the example comparison has a real winner. Every call reports fixed usage, so the token and cost figures can be checked exactly.

## Run it for real

`crates/workbench-cli/evaluations/reference-agent-suite.json` compares `claude-opus-5-5` with `claude-haiku-5-5` on six report tasks, each run twice, with network access to `api.anthropic.com` only:

```sh
export ANTHROPIC_API_KEY=...        # read from your environment, never stored
workbench run crates/workbench-cli/evaluations/reference-agent-suite.json --dry-run
workbench run crates/workbench-cli/evaluations/reference-agent-suite.json
workbench compare REFAGENT-HAIKU REFAGENT-OPUS
```

- **It costs money.** A real run makes billed API calls: 24 trials of a few model calls each. Check the dry run first.
- **Prices.** The suite's `pricing` holds the published per-million-token prices when it was written. Check current prices before trusting the cost figures.
- **Static forwarder.** The Podman runner needs it built once:

  ```sh
  RUSTFLAGS='-C target-feature=+crt-static' cargo build --release -p cybercore-egress-forwarder --target x86_64-unknown-linux-gnu
  ```

  Then set `runner.egress.forwarder_path` if the workbench can't find it.

## Wrap your own agent

Copy this pattern:
1. Read the task from the environment and input files.
2. Write results under `/workspace/output`.
3. Reach the model API through `HTTPS_PROXY`. The workbench sets it, and it's the container's only way out.
4. Before exiting, write `cybercore-usage.json`:

   ```json
   {"format": "cybercore-usage/v0",
    "models": [{"model_id": "claude-opus-5-5", "input_tokens": 8000, "output_tokens": 800}],
    "tool_calls": 3}
   ```

Usage reports are self-reported and the workbench labels them that way. Fill them from the API's `usage` fields, never from estimates.
