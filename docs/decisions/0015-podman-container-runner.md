# ADR-0015: Rootless Podman Container Runner

- **Status:** Accepted (implemented in `evaluation-core::container`, wired into `workbench-cli`)
- **Date:** 2026-11-19
- **Builds on:** ADR-0003 (runner boundary), ADR-0008 (Bubblewrap runner), ADR-0014 (output files)

## Context

The Bubblewrap runner is documented prototype isolation:
- it has no cgroup memory, CPU or process limits;
- it can only run what's in the host's `/usr`;
- its opted-in output directory sits on the host filesystem, so a task can fill the disk.

The owner chose a container runner over a KVM microVM (stronger, but it needs a guest image pipeline and nested-KVM CI) and over limits on Bubblewrap (smaller, but leaves the disk and toolchain gaps). The host has rootless Podman on cgroup v2, with memory, CPU and pids delegated to the user.

## Decision

`ContainerRunner` runs each attempt in its own rootless Podman container.

**Images:**
- trials pick an image from an allowlist (`images`, a logical ID mapped to a local reference);
- each reference is resolved to its **image ID** at construction, and containers run by that ID with `--pull never`, so a moved tag cannot change an evaluation and the runner never downloads anything;
- images must provide `sleep`.

**Lockdown flags:** `--network none`, `--cap-drop all`, `--security-opt no-new-privileges`, `--read-only`, `--userns keep-id`, `--memory` with `--memory-swap` equal to it (no swap), `--cpus`, `--pids-limit`, `--log-driver none`, and `--no-hosts`. Podman's default seccomp profile also applies.

**Mounts:**
- size-limited tmpfs at `/workspace` and `/tmp`;
- inputs bind-mounted read-only at `/workspace/input`;
- for trials with `collect_outputs`, a tmpfs at `/workspace/output` **sized to the trial's `max_artifact_bytes`**, so the kernel enforces the output limit and the disk-fill gap is closed.

**Fail closed on limits.** After the container starts, the runner reads its cgroup `memory.max` and `pids.max`. If they don't match the configuration (for example on a host without cgroup v2 delegation), it refuses to run the task.

**Lifecycle:**
- the container's own process is a `sleep` that outlives the task budget by two minutes, so an orphaned container still expires;
- the task runs via `podman exec`, which returns when the task's main process exits even if background processes remain;
- on a timeout or output-limit kill, the container is removed at once;
- removal is synchronous on every path, including errors and cancellation: `podman rm --force --time 0`, which also handles paused containers, unlike `podman stop` plus `--rm`.

**Output collection.** After a clean exit, the container is **paused** (cgroup freezer), so no process, including leftover background processes, can change files while the host reads them. The host walks `/proc/<pid>/root/workspace/output` with the ADR-0014 rules (no links followed, only regular files, limits) and copies accepted files into staging (`collect_outputs_into`). `--userns keep-id` makes the task run as the host user, so files it creates with mode `0600` are still readable.

**Memory kills.** An OOM kill is detected from the container cgroup's `memory.events` and reported as `memory_limit_exceeded`. Like every execution error, it's never treated as a task failure.

**Results:** the output gains a `container` block with the image reference, image digest, the limits, and `oom_killed`.

**CLI:** evaluation files accept `runner.type: "podman"` with `images`, in-image `allowed_executables`, the shared `limits`, and `resources` (`memory_bytes`, `cpu_millis`, `pids_limit`). Trials pick an image with `task_input.image_id`, which is optional when only one image is configured. Example: `evaluations/container-report.json`.

## Consequences

- Real memory, CPU, process, workspace and output limits, a pinned per-trial toolchain, and no network, tested against real Podman locally and in a dedicated CI job.
- It still shares the host kernel: a kernel or runtime escape is out of scope. The KVM microVM remains the path for hostile multi-tenant workloads.
- As with Bubblewrap, environment values appear in the host-side `podman exec` command line, and output is unredacted, so secrets must not be passed in task inputs.
- Each attempt costs a container start (sub-second locally).
