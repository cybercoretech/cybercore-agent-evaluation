# ADR 0008: Bubblewrap-backed local process runner

- **Status:** Accepted for the initial Linux local runner
- **Date:** 2026-11-17

## Context

The Workbench has a provider-independent Rust `Runner` interface and an in-memory trial coordinator. It needs a concrete local backend for development and controlled evaluations without silently running arbitrary task commands in the host environment.

## Decision

Implement a Linux-only local process runner backed by Bubblewrap. The runner accepts a logical executable ID, arguments, bounded UTF-8 input files, and an explicitly allowlisted task environment. It does not invoke a shell. The sandbox uses isolated user/PID/IPC/UTS/network namespaces, read-only system mounts, ephemeral size-limited tmpfs workspaces, a cleared sandbox environment, and explicit wall-time/output limits. Operator configuration is subject to hard ceilings. Secrets are prohibited in task arguments/environment because those values are present in Bubblewrap's host-side argv.

Only deny-all network and deny-all external side-effect policies are supported. Unsupported policies fail closed. Construction performs an actual Bubblewrap namespace preflight and does not fall back to an unsandboxed process.

## Consequences

- The same Runner API can be used by local and future remote adapters.
- Local development and limited controlled evaluation do not need cloud credentials.
- Runtime availability depends on Bubblewrap and host namespace policy.
- Workspace writes are ephemeral; file artifact export remains deferred until the artifact/evidence-store contract can preserve bytes, hashes, provenance, and redaction status.
- This is not sufficient for hostile multi-tenant workloads because aggregate CPU, memory, and process-count limits are not yet implemented. A disposable VM or a hardened remote execution service is required for that threat model.
- Host-side input staging files are private and removed after normal execution, but crash cleanup and a secret broker are not implemented.
- Non-zero process status does not reliably distinguish child-program exit from every possible per-run Bubblewrap setup failure; diagnosis must use captured stderr and runner diagnostics.

## Alternatives considered

- **Direct host process execution:** rejected because it cannot substantiate deny-all network or external side-effect claims.
- **Shell-based wrapper:** rejected because it introduces a shell parsing boundary and makes argument handling harder to audit.
- **Cloudflare/container runner first:** deferred; the core should have a usable local adapter without cloud credentials.
- **VM-based runner immediately:** deferred to a later high-isolation runner; it has stronger separation but is heavier for the initial local development path.
