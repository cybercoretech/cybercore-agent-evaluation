# Local Process Runner

**Status:** initial Linux implementation; Bubblewrap-backed; not a general-purpose hostile-code VM.

## Purpose

`LocalProcessRunner` is the first concrete implementation of the provider-independent `Runner` port. It launches exactly one configured executable per run attempt. The executable is selected by a logical ID from an operator-maintained allowlist. The task cannot supply an arbitrary executable path or a shell command string.

The runner is deliberately Linux-only and requires Bubblewrap (`bwrap`) plus working unprivileged user, PID, IPC, UTS, and network namespaces. On Debian/Ubuntu-family systems, Bubblewrap is commonly installed with `sudo apt install bubblewrap`. Construction performs a real namespace preflight. If that preflight fails, construction fails; the runner never falls back to an ordinary host process.

## Task input

`ExecutionRequest.task_input` must be an object with this shape:

```json
{
  "executable_id": "python3",
  "args": ["-c", "print('hello')"],
  "files": [
    {"path": "fixtures/input.txt", "content": "fixture content"}
  ],
  "environment": {
    "APP_MODE": "evaluation"
  }
}
```

- `executable_id` must exist in `LocalProcessRunnerConfig.allowed_executables`.
- Arguments are passed as an argument vector. No shell is invoked and no shell interpolation occurs.
- Input files are UTF-8, size-limited, created in a private temporary directory, and mounted read-only at `/workspace/input`.
- Input paths must be relative POSIX paths. Absolute paths, `.`/`..`, backslashes, duplicate paths, and conflicting file/directory paths are rejected.
- Task environment variables must be named in the operator's `allowed_environment_keys`. Loader and shell-control variables such as `LD_*`, `DYLD_*`, `PATH`, `HOME`, and `IFS` are reserved and cannot be overridden.
- Total arguments are limited to 512 KiB, each argument to 64 KiB, and each input path to 4096 bytes with 255-byte components.
- The operator's hard configuration ceilings are 60 minutes wall time, 64 MiB captured output, 256 MiB workspace tmpfs, 64 MiB `/tmp` tmpfs, 256 MiB aggregate input files, 1024 files, 4096 arguments, and 512 KiB aggregate argument bytes.

The working directory is `/workspace`. The process can copy read-only input files into its writable scratch workspace.

## Enforced boundary

Bubblewrap is launched with:

- A new user namespace.
- New PID, IPC, UTS, and network namespaces.
- `--die-with-parent` and `--new-session`.
- Dropped Linux capabilities.
- The host `/usr` tree and merged-usr aliases mounted read-only.
- A fresh `/dev` and `/proc` inside the sandbox.
- A size-limited tmpfs for `/workspace` and another size-limited tmpfs for `/tmp`.
- No host home directory, host project directory, host `/run`, or host sockets mounted into the sandbox.
- A cleared environment followed by a small fixed environment (including a sandbox-correct `PWD=/workspace`) and the explicitly approved task variables.

`NetworkPolicy::DenyAll` and `SideEffectPolicy::DenyAll` are the only accepted policies. Network allowlists and side-effect allowlists are rejected. Writes are limited to the ephemeral tmpfs workspace and `/tmp`; they do not persist on the host after the run. The input mount is read-only.

The host-side input staging directory is created with mode 0700 and staged input files with mode 0600; it is removed after normal execution. A host crash or forced termination can leave a stale staging directory, so operators must manage the configured temporary root. Stdout and stderr are read concurrently under one combined output-byte budget. If the process exceeds the capture limit, the Bubblewrap process is terminated. A wall-clock timeout also terminates the sandbox process. After the process exits (normally or after termination), stdout and stderr get a fixed 2-second drain grace period to close; a stream still open after that is abandoned, so a descendant holding a pipe cannot stall the runner past its budget.

Do not pass credentials, API keys, or other secrets in process arguments or task environment values. Bubblewrap receives those values as host-side command-line arguments, which may be visible to same-user or privileged host process inspection. The temporary input staging files also exist briefly on the host filesystem. Stdout/stderr are captured without redaction. A credential broker or secret-injection mechanism is not implemented.

## Result semantics

The result includes:

- `execution_started` and terminal execution events.
- Exit code when available.
- Captured stdout and stderr as UTF-8-lossy strings. Output is not redacted in this version; do not use it with secrets or sensitive data that must be filtered before capture.
- `output_truncated` to disclose byte-limited capture.
- `output_drain_timed_out` when a stream did not close within the drain grace period; that stream's capture is discarded and reported as empty and truncated.
- `unmeasured_usage_fields` marks token and tool-call usage as unavailable. Their zero-valued fields in the shared `ResourceUsage` DTO are placeholders, not measured zero usage.
- Measured wall time and captured output bytes (the retained byte count; `output_truncated` discloses when more output was emitted than retained).
- `Finished` only when the process exits with code zero.
- `Errored` for non-zero exit, output-limit termination, or a drain timeout (failure code `output_drain_timed_out`).
- `TimedOut` for wall-clock termination.

The concrete adapter requires a Tokio runtime for asynchronous process I/O. A zero exit code is only an execution result. It does not mean the task passed. Independent grading and verification remain separate.

## Explicit non-goals / limitations

- Workspace file outputs are ephemeral and are **not** exported as `ArtifactManifest` records in this version. Use stdout/stderr for returned data until the artifact/evidence-store adapter is implemented. Do not claim persistent artifact capture.
- No cgroup CPU/memory/process-count quotas are applied by this first version. tmpfs size limits bound writable filesystem capacity, not aggregate memory use, CPU consumption, or process count. Concurrent runner instances can multiply host resource pressure. Do not use this runner for hostile multi-tenant workloads or arbitrary public submissions.
- Bubblewrap namespaces and mount isolation reduce host access but do not substitute for a disposable VM or a separately managed container runtime with cgroup resource controls and a reviewed seccomp profile.
- The initial executable is allowlisted, but the read-only `/usr` runtime tree is visible; a process may invoke other binaries present there. Do not treat the executable ID allowlist as a complete command-execution policy. The allowlist constrains the initial exec target only.
- A non-zero Bubblewrap exit is reported as a non-zero process exit. The normalized result does not yet reliably distinguish a child-program exit from every possible per-run Bubblewrap setup/exec failure; inspect captured stderr and runner diagnostics before attributing the failure to the agent.
- There is no durable workspace, file artifact export, input-token accounting, tool-call accounting, or process-tree telemetry.
- No Windows/macOS implementation is provided.

For hostile code, use a disposable VM or a separately hardened execution service with aggregate resource controls. Never weaken this runner's policy to make an unsupported host run.

## Example configuration

```rust
use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use cybercore_evaluation_core::LocalProcessRunnerConfig;

let config = LocalProcessRunnerConfig {
    runner_id: "local-bwrap-primary".into(),
    runner_version: "0.3.0".into(),
    bubblewrap_path: PathBuf::from("/usr/bin/bwrap"),
    temporary_root: PathBuf::from("/var/tmp/cybercore-runner-inputs"),
    allowed_executables: BTreeMap::from([
        ("python3".into(), PathBuf::from("/usr/bin/python3")),
    ]),
    allowed_environment_keys: BTreeSet::from(["APP_MODE".into()]),
    max_wall_time_ms: 30_000,
    max_output_bytes: 1_048_576,
    max_workspace_bytes: 268_435_456,
    max_tmp_bytes: 67_108_864,
    max_input_file_bytes: 8_388_608,
    max_input_files: 128,
    max_arguments: 256,
    max_argument_bytes: 65_536,
};
```

`LocalProcessRunner::new` performs a Bubblewrap preflight and returns an error if the host cannot create the requested namespaces. Call `execute_with_runner` so the shared request/result policy validation is applied before and after execution.

Run the repository's real process smoke example after installing Bubblewrap:

```sh
cargo run --manifest-path crates/evaluation-core/Cargo.toml --example local_process_runner_smoke
```

The example reads a supplied fixture through the read-only input mount using `/usr/bin/cat`. Set `BWRAP` if the Bubblewrap binary is not at `/usr/bin/bwrap`.
