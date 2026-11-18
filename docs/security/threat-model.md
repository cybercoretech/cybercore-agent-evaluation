# Security, Privacy, and Tenant Boundaries

**Version:** 0.1 — initial threat model  
**Status:** design baseline; full threat analysis and deployment controls are required before hosted alpha

## 1. Assets to protect

- Source code, system prompts, private skills, benchmark tasks, hidden graders, fixtures, and private corpora.
- Agent traces, tool arguments/results, terminal output, generated files, screenshots, and reports.
- API credentials, model-provider keys, repository tokens, cloud credentials, and user identities.
- Evaluation decisions, capability passports, attribution findings, and intervention records.
- Workspace/project boundaries, audit records, deletion requests, and backups.

## 2. Trust boundaries

1. Browser/CLI/MCP client to API or local service.
2. API adapter to application services and authorization policy.
3. Application services to D1/R2/Queues/Workflows.
4. Control plane to runner.
5. Runner to agent, tools, filesystem, network, and external provider.
6. Imported artifact parser/renderer to the UI and analysis engine.
7. Hosted alpha workspace to any future external workspace.

Treat imported traces and agent-produced files as untrusted input. They may contain prompt injection, hostile markup, oversized payloads, malformed serialization, secrets, or malicious archives.

## 3. Runner isolation

The runner is the primary execution security boundary. The orchestration/control plane must not execute arbitrary evaluated code with its own credentials or filesystem access.

Minimum requirements for any runner implementation:

- Deny-by-default network egress; explicit allowlists and observable policy decisions.
- No unrestricted production credentials in agent environments.
- Short-lived, narrowly scoped credentials only when the task requires them.
- Separate filesystem/workspace per run; explicit mount and write permissions.
- CPU, memory, wall-clock, process-count, output-size, and storage quotas.
- Kill, timeout, cleanup, and rollback behavior.
- Explicit approval for meaningful external side effects.
- Record runner image/version, policy, limits, and actual violations.
- Treat tool outputs and generated artifacts as untrusted.

A Cloudflare Container or sandbox option is not automatically approved for every workload. Validate isolation, egress, secret delivery, quotas, persistence, and failure cleanup before use.

## 4. Data handling

- Redact credentials, tokens, personal data, private source, and unnecessary content before persistence or export.
- Separate raw evidence from extracted/derived summaries.
- Keep R2 buckets private; never expose public object URLs as an access-control mechanism.
- Authorize every metadata and artifact operation against the resolved workspace/project.
- Use short-lived scoped access for downloads when direct object access is necessary; otherwise proxy through an authorization-checking service.
- Do not put secrets in IDs, URLs, logs, error messages, benchmark labels, or model-judge prompts.
- External model judging requires explicit user approval, minimized/redacted inputs, provider disclosure, and a record of what was sent.
- Render Markdown/HTML and other uploaded formats safely; escape active content and avoid executing embedded scripts.

## 5. Identity and authorization

The alpha has no public self-service signup. Use an approved identity/access boundary for the small set of permitted operators. Design an identity-provider port so local and hosted modes can differ without changing domain authorization rules.

Authorization is enforced inside application services, not only in the UI or MCP catalog. Every read, write, artifact access, export, job start, approval, and intervention is checked against principal, workspace, project, action, and resource state.

Future tenant-ready modeling requires `workspace_id` and project scoping on records, indexes, and storage references. That is only a data-model prerequisite. Do not claim tenant isolation until cross-tenant access, storage partitioning, background job scoping, cache isolation, audit, deletion, and backup/restore have been tested.

## 6. High-risk operations and human approval

Require explicit approval according to policy for:

- Running untrusted code or enabling network egress.
- Tasks that write to external systems or repositories.
- Using sensitive data or uploading traces to external providers.
- Starting or widening a controlled intervention.
- Changing permission policies, credentials, retention, or deletion settings.
- Publishing a capability passport or external readiness assertion.

MCP tool exposure must make available operations visible and should preserve human control for risky tool invocations. Read-only query tools can be separated from mutating tools. Tool names and descriptions must not imply broader authority than authorization grants.

## 7. Audit requirements

Record actor, authorization decision, timestamp, action, target resource, request/correlation ID, before/after revision references, approval reference, and result for sensitive actions. Avoid copying sensitive payloads into the audit log.

Audit events are append-oriented. Tamper-evidence or signatures may be needed for external attestations or high-assurance deployments; a content hash alone does not authenticate the actor.

## 8. Required security tests before hosted alpha

- Cross-workspace and cross-project IDOR tests for every resource family.
- Artifact URL/object-key guessing and stale-link tests.
- Queue/workflow message poisoning and replay tests.
- Authorization re-check after permission revocation.
- Runner network-egress, filesystem, secret, process, and resource-limit tests.
- Prompt injection embedded in imported traces, tool output, HTML, Markdown, archives, and task fixtures.
- Oversized, malformed, decompression-bomb, and polyglot artifact tests.
- Redaction tests for structured and unstructured secrets.
- Deletion, backup, restore, and retention behavior tests.
- MCP authorization and human-approval tests for every mutating tool.
