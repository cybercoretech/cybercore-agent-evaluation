# 🔬 CyberCore Agent Evaluation Workbench

**Status:** contract foundation, Python/TypeScript/Rust conformance, initial Rust domain core, in-memory evaluation/trial orchestration with durable local scheduling (fenced write-ahead journal, ADR-0010) that records each resolved attempt as `run`/`run_event`/`verification_result` records judged by a pluggable independent verifier (ADR-0011), concurrent attempts and task-revision outcome rules (ADR-0013), and a local `workbench` CLI to run and inspect evaluations (ADR-0012), a Bubblewrap-backed Linux local process runner prototype, and local persistence (SQLite metadata + content-addressed evidence store, `crates/evaluation-store`) implemented. Hosted scheduling, hosted D1/R2 persistence adapters, runner artifact export, production-grade hostile-code isolation, API, MCP, and UI adapters remain unimplemented.

The CyberCore Agent Evaluation Workbench is a provider-independent platform for designing, executing, verifying, analyzing, and improving AI-agent evaluations. Its signature capability is the Failure Attribution Engine: evidence-linked causal diagnosis, intervention validation, and cross-run reliability analysis.

The Workbench is a companion to, not a dependency of, the [CyberCore Agent Skills](https://github.com/cybercoretech/cybercore-agent-skills) project. The skill supplies portable methodology and operating guidance. The Workbench supplies persistent records, controlled execution, evidence management, analysis, and reports. Both remain independently useful.

## 🧭 Initial product posture

- Public open-source repository.
- Private hosted alpha for CyberCore use; no public self-service signup initially.
- Local-first CLI and evaluation workflow must not require Cloudflare credentials.
- Cloudflare is the initial hosted control-plane target, behind provider interfaces.
- Future workspace/tenant scoping is designed into records, but production multi-tenant isolation is not claimed or enabled by this design alone.
- The Rust core provides shared domain-invariant validation, run-state projection, a provider-independent runner port, an in-memory evaluation/trial coordinator, and a Linux Bubblewrap local process runner prototype. The local runner is not a hardened VM: it has no aggregate cgroup memory/CPU/process-count quotas and does not export workspace files as artifacts. It must not be used for hostile public workloads. Do not pass secrets in task arguments or environment values because they are passed in the host-side Bubblewrap command line; captured stdout/stderr are also unredacted. Local persistence is provided by `crates/evaluation-store` (append-only SQLite records and a content-addressed evidence store); durable local scheduling is provided by `DurableScheduler` in the same crate; the local `workbench` CLI runs and inspects evaluations; no hosted scheduler, hosted persistence adapter, API endpoints, MCP tools, or hosted service are implemented yet.

## 🗺️ Start here

1. [System architecture](docs/architecture/system-architecture.md)
2. [Domain model](docs/architecture/domain-model.md)
3. [Execution and evidence](docs/architecture/execution-and-evidence.md)
4. [Lifecycle state machines](docs/architecture/state-machines.md)
5. [Security and tenancy](docs/security/threat-model.md)
6. [Contract strategy](docs/contracts/README.md)
7. [Rust evaluation core](docs/architecture/RUST-EVALUATION-CORE.md)
8. [Evaluation orchestration](docs/architecture/EVALUATION-ORCHESTRATION.md)
9. [Runner protocol](docs/architecture/RUNNER-PROTOCOL.md)
10. [Local process runner](docs/architecture/LOCAL-PROCESS-RUNNER.md)
11. [Local persistence](crates/evaluation-store/README.md) ([ADR-0009](docs/decisions/0009-local-persistence.md))
12. [`workbench` CLI](crates/workbench-cli/README.md) ([ADR-0012](docs/decisions/0012-local-workbench-cli.md))
13. [Architecture decision records](docs/decisions/)
14. [Current platform notes and source links](docs/research/current-platform-notes.md)

## 🧪 Development setup

Contract and core checks need Python 3.10+, Node.js 22+ with npm, and a stable Rust toolchain (1.85 or newer) with `clippy` and `rustfmt`.

```sh
python3 -m venv .venv
. .venv/bin/activate
pip install -r requirements-contracts.txt

bash scripts/validate_all_contracts.sh
cargo fmt --all --check
cargo clippy --workspace --all-targets --locked -- -D warnings
```

`validate_all_contracts.sh` runs the Python, TypeScript and Rust schema and domain-invariant conformance checks and the core unit tests. It installs the TypeScript harness with `npm ci`, so `contracts/conformance/typescript/package-lock.json` is never rewritten. CI runs the same commands on every pull request.

The real-process smoke example additionally needs Bubblewrap (`bwrap`) and unprivileged user namespaces; see the [evaluation core README](crates/evaluation-core/README.md).

## 🛡️ Design rule

Do not expose API, CLI, or MCP operations until the underlying application services, persistence, authorization behavior, and security boundaries are implemented and tested. Do not treat process exit status as task success; independent verification remains mandatory.
