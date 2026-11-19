# ADR-0019: Allowlisted Egress and Granted Credentials for Container Runs

- **Status:** Accepted (implemented in `evaluation-core::egress`, `ContainerRunner::with_egress`, the `cybercore-egress-forwarder` crate and `workbench-cli`)
- **Date:** 2026-11-19
- **Builds on:** ADR-0015 (Podman container runner), RUNNER-PROTOCOL (network policies)

## Context

Every run had no network at all. A real AI agent usually has to call a model API while it works, so it could not be evaluated. The contracts already model `network_policy: {mode: allowlist, hosts: [...]}`, but no runner enforced it. The agent also needs an API key, and the key must not leak into the evaluation file, command lines, the journal or stored evidence.

## Decision

- **The container keeps `--network none`.** It has only loopback. Its one way out is a host-side **egress proxy** (`EgressProxy`) inside the workbench process, reached through a Unix socket that is bind-mounted read-only into the container. With no network interface, there is no route to bypass.
- **The proxy accepts only `CONNECT host:port`.** The host must be an exactly allowlisted DNS name (case-insensitive, no wildcards) and the port an allowed one (default 443). The proxy also:
  - refuses IP literals and plain HTTP;
  - resolves names itself and refuses names that resolve only to loopback, private, link-local, CGNAT or other non-public addresses, which blocks DNS rebinding into host services (`allow_private_destinations` exists for tests only);
  - connects to the address it validated, so there is no gap between check and use.
- **SNI check.** After the tunnel opens, the proxy reads the TLS ClientHello and requires its server name to equal the requested host. Non-TLS traffic and domain fronting through a shared CDN address are refused. Encrypted ClientHello (ECH) whose outer name differs is refused too, which is acceptable.
- **Limits.** Connection attempts (allowed or denied) are capped per run; attempts over the cap are dropped unread and only counted, so a flood can't grow the log or the thread count. Bytes are capped per run across both directions.
- **Logging.** Every attempt is logged (host, port, allowed or denied with the reason, bytes up and down) in the run output under `network.egress`. Final event payloads carry the allowed and denied counts.
- **In-container forwarder.** Most HTTP clients only speak to TCP proxies, so a tiny forwarder (`cybercore-egress-forwarder`, standard library only) listens on `127.0.0.1:3128` *before* starting the task. It relays each connection to the socket and runs the task with `HTTP(S)_PROXY`/`ALL_PROXY` set. It makes no policy decisions; killing or bypassing it only loses the task its own access. It must be **statically linked** to run in any image. `with_egress` checks that the ELF has no interpreter and refuses otherwise, with the build command in the error.
- **Socket directory.** Unix socket paths are limited to 108 bytes, so sockets live in short-named, owner-only directories under `$XDG_RUNTIME_DIR/cybercore-egress` (falling back to the temporary root). `with_egress` refuses roots that are too long.
- **Credentials are granted, never written down.**
  - The runner holds named credentials; the CLI reads them from host environment variables (`credentials.NAME.from_env`), and a missing variable is an error.
  - A trial receives only the names it lists in `task_input.credentials`, and only with an allowlist policy.
  - Values reach the container through `podman exec --env NAME`, which podman copies from its own environment, so they never appear in an argument list.
  - Granted values are replaced with `[REDACTED:NAME]` in captured stdout and stderr, and a collected output file containing one is removed and listed under `rejected_output_entries`.
  - `ContainerEgressConfig`'s `Debug` output prints only the names.
- **Opt-in builder.** `ContainerRunner::with_egress` is an opt-in builder, so `ContainerRunnerConfig` and existing callers are unchanged. Without it, an allowlist request is `unsupported_policy`.
- **CLI file format.** `runner.egress` on the Podman runner. Validation:
  - an allowlist trial requires `runner.egress`;
  - a trial's credentials require an allowlist policy;
  - every listed credential must be granted;
  - a secret `value` field is rejected as unknown.

  The dry run shows each trial's network and credentials. Example: `evaluations/container-egress.json`.

## Consequences

- Agents that call a model API can now be evaluated inside the container runner, and every connection they make is on the record.
- **Remaining limits:**
  - Redaction matches exact values. A secret that the agent encodes (base64, split across writes), or that is cut off at the output capture limit, can still leak in transformed or partial form.
  - The allowlisted service itself sees whatever the agent sends.
  - While the run is active, the secret is visible to the same host user in the `podman exec` process environment.
  - The Bubblewrap runner still supports deny-all only.
- **Testing:** CI exercises the whole path with real Podman against a loopback TLS stub, so no test traffic leaves the runner. The shipped example reaches `api.anthropic.com` only as far as a TLS handshake, never an API request, so it costs nothing.
