# 🔬 CyberCore Agent Evaluation Workbench

A provider-independent platform for designing, running, verifying and analysing AI-agent evaluations, with every result kept as a contract-validated record and backed by evidence.

The Workbench is a companion to, not a dependency of, the [CyberCore Agent Skills](https://github.com/cybercoretech/cybercore-agent-skills) project. The skill supplies portable methodology and operating guidance; the Workbench supplies controlled execution, persistent records, evidence, analysis and reports. Each is useful on its own.

## 📍 Status

**Local-first and usable today.** The `workbench` CLI runs real agents in sandboxes, verifies their work independently, and turns the results into statistics, comparisons, failure analysis and readiness decisions, all stored locally. **No hosted service, API or MCP tools exist yet.**

### ✅ Works today

| Area | What you get |
|---|---|
| 🏃 **Run** | A rootless **Podman** container runner with enforced memory, CPU and process limits ([ADR-0015](docs/decisions/0015-podman-container-runner.md)), and a **Bubblewrap** runner prototype ([ADR-0008](docs/decisions/0008-bubblewrap-local-runner.md)). Allowlisted network access with granted credentials, for agents that call a model API ([ADR-0019](docs/decisions/0019-egress-allowlist.md)). Crash-safe scheduling that resumes where it stopped ([ADR-0010](docs/decisions/0010-durable-scheduling.md), [ADR-0016](docs/decisions/0016-scheduler-snapshots.md)). |
| 🧩 **Describe** | **Task suites:** list tasks and agents once, and get one evaluation per agent with repeats ([ADR-0024](docs/decisions/0024-task-suites.md)). Or write evaluation files by hand. |
| ✔️ **Verify** | Independent verifiers: exact checks on output and files ([ADR-0014](docs/decisions/0014-workspace-output-files.md)), a Claude model judge for rubric criteria ([ADR-0017](docs/decisions/0017-model-judge-verifier.md)), or both combined ([ADR-0018](docs/decisions/0018-composite-verifier.md)). Outcome rules come from task revisions ([ADR-0013](docs/decisions/0013-concurrency-and-outcome-rules.md)). |
| 📊 **Analyse** | Pass rates with intervals ([ADR-0020](docs/decisions/0020-cross-run-summaries.md)). Task-paired comparisons, **confirmatory** when declared in a frozen plan ([ADR-0021](docs/decisions/0021-predeclared-comparisons.md)). Wall time, agent-reported tokens and cost ([ADR-0022](docs/decisions/0022-speed-and-cost-metrics.md)). Reliability across repeats ([ADR-0029](docs/decisions/0029-reliability-across-repeats.md)). Failure analysis ([ADR-0027](docs/decisions/0027-failure-analysis.md)). |
| 🎫 **Decide** | Readiness decisions and capability passports from gates **declared in advance**, signed by a named reviewer, with an expiry ([ADR-0028](docs/decisions/0028-readiness-passports.md)). |
| 📝 **Share** | One-page Markdown or self-contained HTML reports ([ADR-0026](docs/decisions/0026-workbench-report.md)). |
| 🤖 **Real agents** | A reference Claude SDK agent and a fake Messages API, so the whole real-agent path is tested in CI at no cost ([ADR-0025](docs/decisions/0025-reference-agent.md)). |
| 📜 **Contracts** | v0 JSON Schema contracts with Python, TypeScript and Rust conformance suites ([ADR-0001](docs/decisions/0001-contract-first.md)). Local persistence in SQLite plus a content-addressed evidence store ([ADR-0009](docs/decisions/0009-local-persistence.md)). |

### 🚧 Not built yet

- Hosted scheduling, the hosted service, and Cloudflare D1/R2 persistence adapters.
- The HTTP API, MCP tools and a UI.
- Hardened isolation for hostile, multi-tenant workloads (for example a VM runner).
- Root-cause attribution (the planned Failure Attribution Engine). Failure records exist, but nothing builds causal diagnoses from them yet.

## 🚀 Quick start

```sh
cargo build --release -p cybercore-workbench-cli
W=./target/release/workbench

# Two shell "agents" on six tasks, each run twice (needs Bubblewrap)
$W run crates/workbench-cli/evaluations/report-suite.json --dry-run
$W run crates/workbench-cli/evaluations/report-suite.json

$W summary REPORTS-CAREFUL                       # pass rate, speed, cost, reliability
$W compare REPORTS-SLOPPY REPORTS-CAREFUL        # paired, confirmatory (declared in advance)
$W failures REPORTS-SLOPPY                       # what went wrong, and where
$W passport REPORTS-CAREFUL --reviewer "Your Name"
$W report REPORTS-SLOPPY REPORTS-CAREFUL --format html > report.html
```

The [`workbench` CLI README](crates/workbench-cli/README.md) covers every command and file format. To evaluate a real model-backed agent, start from the [reference agent](examples/reference-agent/README.md).

## 🧱 Repository layout

| Path | What it is |
|---|---|
| `contracts/` | v0 JSON Schemas, fixtures and the Python, TypeScript and Rust conformance harnesses |
| `crates/evaluation-core` | Domain core: runner port, orchestration, the Bubblewrap and Podman runners, egress proxy |
| `crates/evaluation-store` | Local persistence, durable scheduler, verifiers, and analysis (summaries, comparisons, failures, readiness, reliability) |
| `crates/evaluation-judge` | The Claude model-judge verifier |
| `crates/egress-forwarder` | Static in-container forwarder for allowlisted network access |
| `crates/workbench-cli` | The `workbench` command line, with example evaluations and suites in `evaluations/` |
| `examples/reference-agent` | A reference Claude SDK agent, its container recipe and a fake Messages API |
| `docs/` | Architecture, contracts, security notes and [decision records](docs/decisions/) |

## 🛡️ Isolation and safety

- **Both runners share the host kernel.** The Podman runner enforces cgroup limits, a read-only image and size-capped output. The Bubblewrap runner is a prototype with no aggregate memory, CPU or process quotas, and its opted-in output directory isn't size-limited during a run. Neither is suitable for hostile public workloads.
- **Secrets never go in task inputs.** Grant credentials through the Podman runner's egress settings instead. Granted values are redacted from captured output, and output files containing them are rejected. Other output is stored unredacted, so only store output evidence for non-sensitive tasks.
- **Tokens and cost are self-reported** by the agent and labelled that way everywhere. Wall time is measured by the workbench.
- **Process exit status is never treated as task success.** Independent verification is mandatory.

## 🧭 Product posture

- Public open-source repository.
- Private hosted alpha for CyberCore use; no public self-service signup at first.
- The local CLI and workflow never require Cloudflare credentials.
- Cloudflare is the initial hosted control-plane target, behind provider interfaces.
- Workspace and tenant scoping is designed into the records, but production multi-tenant isolation is not claimed.

## 🗺️ Documentation

1. [System architecture](docs/architecture/system-architecture.md), [domain model](docs/architecture/domain-model.md) and [lifecycle state machines](docs/architecture/state-machines.md)
2. [Execution and evidence](docs/architecture/execution-and-evidence.md), [runner protocol](docs/architecture/RUNNER-PROTOCOL.md) and [local process runner](docs/architecture/LOCAL-PROCESS-RUNNER.md)
3. [Rust evaluation core](docs/architecture/RUST-EVALUATION-CORE.md) and [evaluation orchestration](docs/architecture/EVALUATION-ORCHESTRATION.md)
4. [Contract strategy](docs/contracts/README.md), [schema catalog](docs/contracts/SCHEMA-CATALOG.md) and [metrics and comparisons](docs/contracts/METRICS-AND-COMPARISONS.md)
5. [Security and tenancy](docs/security/threat-model.md)
6. [Local persistence](crates/evaluation-store/README.md) and the [`workbench` CLI](crates/workbench-cli/README.md)
7. [Architecture decision records](docs/decisions/) (ADR-0001 to ADR-0029)
8. [Current platform notes and source links](docs/research/current-platform-notes.md)

## 🧪 Development setup

You need Python 3.10+, Node.js 22+ with npm, and a stable Rust toolchain (1.85 or newer) with `clippy` and `rustfmt`.

```sh
python3 -m venv .venv
. .venv/bin/activate
pip install -r requirements-contracts.txt

bash scripts/validate_all_contracts.sh
cargo fmt --all --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked
```

- **Contract checks.** `validate_all_contracts.sh` runs the Python, TypeScript and Rust schema and domain-invariant checks. It installs the TypeScript harness with `npm ci`, so the lockfile is never rewritten.
- **Real-execution tests** skip when their tools are missing. Set the matching variable to make a skip fail instead, as CI does:
  - `WORKBENCH_REQUIRE_BWRAP=1` needs Bubblewrap and unprivileged user namespaces (see the [evaluation core README](crates/evaluation-core/README.md));
  - `WORKBENCH_REQUIRE_PODMAN=1` needs rootless Podman with `docker.io/library/ubuntu:22.04` pulled;
  - `WORKBENCH_REQUIRE_REFERENCE_AGENT=1` needs the reference agent image built: `podman build -t localhost/cybercore-reference-agent:0.1 examples/reference-agent`.
- **No real model calls.** No test or CI job calls a real model API; real-agent tests use the fake Messages API.

## 🛡️ Design rule

Don't expose API or MCP operations until the underlying application services, persistence, authorization and security boundaries are implemented and tested. Never treat process exit status as task success; independent verification stays mandatory.
