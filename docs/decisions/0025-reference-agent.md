# ADR-0025: Reference Agent and Fake Messages API

- **Status:** Accepted (implemented in `examples/reference-agent`, `evaluations/reference-agent-suite.json` and a Podman CLI test)
- **Date:** 2026-11-19
- **Builds on:** ADR-0019 (egress), ADR-0022 (usage and pricing), ADR-0024 (task suites)

## Context

Each piece of the workbench was tested on its own: allowlisted egress against a TLS stub, usage files written by shell scripts, suites with shell "agents". But nothing showed or proved that a real model-backed agent works end to end. An SDK agent has to call the API through the proxy with a granted key, use tools inside the sandbox, and report its usage. The owner chose a small reference agent over wrapping Claude Code: it's fully testable, and it's a template for wrapping any agent.

## Decision

- **`agent.py`** uses the official Python SDK, pinned to `anthropic==2.5.0` (released 2026-11-04):
  - **Loop.** It runs the SDK's tool runner with `claude-opus-5-5` by default (`AGENT_MODEL` to change it) and adaptive thinking.
  - **Tools.** Three file tools are confined by resolved path: read from `/workspace/input` and `/workspace/output`, write only to `/workspace/output`. The usage file is reserved.
  - **Usage report.** It writes `cybercore-usage.json` in a `finally` block, from each response's `usage`. Cache read and write tokens count as input, which overstates cost slightly when caching is used.
  - **Errors.** An API error prints only the error type and status, never request details.
  - **Fake-API hooks.** `ANTHROPIC_BASE_URL` and `AGENT_CA_FILE` are used only for the fake API.
- **`Containerfile`**: `python:3.12-slim` plus the pinned SDK plus `agent.py`, built as `localhost/cybercore-reference-agent:0.1`. The runner still never pulls; CI builds the image in the Podman job.
- **`fake_api.py`** is standard library only and serves HTTPS on loopback with a throwaway certificate:
  - **Auth.** It requires the expected key, sent as `x-api-key` or as a bearer token; the SDK sends a bearer token. It logs only header names and lengths on a 401.
  - **Message threads.** It implements them, because SDK 2.x's tool runner sends only new messages and continues the stored conversation (`thread.previous_message_id`). An unknown thread returns 404 `thread_not_found`, as the real API does.
  - **Script.** A fixed sequence (list files, read the `.txt`, write the total, finish) with fixed usage per call. A model ID containing `haiku` plays a weaker model that writes the line count.
- **`reference-agent-suite.json`** compares Haiku 5.5 with Opus 5.5 on six report tasks, each run twice, with egress to `api.anthropic.com` only, a granted `ANTHROPIC_API_KEY`, declared prices and a predeclared comparison.
- **The test** (`reference_agent_runs_through_egress_against_the_fake_api`) takes the shipped suite and points it at the fake: hosts `localhost`, the fake's port, private destinations allowed, the fake's certificate added as an input file. It runs the suite in real Podman and checks:
  - pass counts of 12 and 2, with a confirmatory comparison;
  - exact token and cost figures (8,800 tokens and $0.048 per Opus trial);
  - that the key never reaches the store.

  CI requires it to run (`WORKBENCH_REQUIRE_REFERENCE_AGENT=1`). Locally it skips when the image isn't built.

## Consequences

- The whole path is proven in CI with no API traffic and no cost: SDK agent → forwarder → egress proxy with SNI check → granted key → tool use in the sandbox → usage file → verification → summaries and comparison.
- A real run needs an API key and costs money; the README says so, and says to check prices.
- **The fake covers only the reference agent's request shapes.** Other agents may need more (streaming, other endpoints).
- **SDK upgrades are deliberate.** Bumping `anthropic` means rebuilding the image and re-running the test.
