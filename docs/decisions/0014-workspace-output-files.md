# ADR-0014: Collecting and Verifying Workspace Output Files

- **Status:** Accepted (implemented in `evaluation-core`, `evaluation-store`, `workbench-cli`)
- **Date:** 2026-11-19
- **Builds on:** ADR-0008 (Bubblewrap runner), ADR-0011, ADR-0013

## Context

Verification could only inspect stdout. Many tasks are judged by the files an agent writes, but the local runner's `/workspace` is a tmpfs that disappears with the sandbox (`workspace_file_artifact_export: not_implemented`).

## Decision

### Runner

- **Opt-in per trial.** With `task_input.collect_outputs: true`, a fresh per-run host directory is bind-mounted read-write at `/workspace/output`; everything else stays tmpfs or read-only. It requires the runner's `artifact_root` and a positive `max_artifact_bytes` budget; otherwise the request is rejected before launch.
- **Collect only after a clean exit.** When the process ends on its own, the kernel has already killed and reaped every process in the sandbox's PID namespace (`zap_pid_ns_processes`), so nothing can change the files while the host reads them. After a timeout, output-limit kill or drain timeout, PID 1 dies asynchronously, so those runs' files are discarded (`skipped_after_termination`).
- **Never follow links.** The walk uses `symlink_metadata` and opens files with `O_NOFOLLOW`. Symlinks, FIFOs, sockets and devices are rejected and reported in `rejected_output_entries` instead of being read. Hard links to host files cannot exist there (`EXDEV` across mounts). Non-UTF-8 names and nesting deeper than 32 are rejected too.
- **Limits.** Total bytes are capped by the trial's `max_artifact_bytes`, and the file count by `max_output_files`. Exceeding either fails the attempt with `artifact_limit_exceeded`, and all of that run's files are discarded.
- **Manifests and staging.** Accepted files become `ArtifactManifest`s (relative path, length, SHA-256). They are staged at `artifact_staging_dir(root, request_id)`, a directory named by the SHA-256 of the request ID, so request IDs cannot escape the root.

### Recorder and verification

- Staged files are read only if they are regular files whose length and digest match their manifest; a tampered or missing copy is treated as absent. They are passed to verifiers as `VerificationInput.files`.
- Under the existing `store_output_evidence` attestation, each file is stored as an `evidence_artifact` (`<run>:FILE:<n>`, with its path in `description`). Each verdict cites the evidence of the files it inspected (`CriterionVerdict.evidence_paths`); stdout verdicts cite the stdout evidence; otherwise the terminal `run_event` is cited.
- The staged copy is removed after the records are written. It survives a crash, so catch-up still records it.
- **`ChecksVerifier`** supports several criteria per trial: `stdout_equals`, `stdout_contains`, `file_exists`, `file_absent`, `file_equals` and `file_contains`. File checks are `unavailable` when outputs were not collected. `file_absent` fails if anything, even a rejected symlink, was left at the path. With ADR-0013, each criterion's criticality comes from the task revision.

### CLI

- **New verifier type.** `verifier.type: "checks"` maps each trial ID to a list of `{criterion_id, kind, ...}` criteria.
- **Validation:** criterion IDs must be well-formed and unique per trial; paths must be relative without `..`; file checks require the trial's `collect_outputs`; and a collecting trial's artifact budget must be positive and within `runner.limits.max_artifact_bytes`.
- **Staging location:** `<store>/runner-artifacts`, used only when some trial collects outputs.
- **Example:** `evaluations/report-files.json`.

## Consequences

- Tasks can be judged by the files they leave, with digests and evidence records, and the whole path is tested in the real sandbox in CI.
- **Trade-off:** the output directory is on the host filesystem, so an opted-in task can fill the disk during its run. A tmpfs couldn't, but its contents can't outlive the sandbox. This is the same class of gap as the runner's missing cgroup memory, CPU and process quotas; it only applies to opted-in trials, and the runner remains documented as unsuitable for hostile code.
- Runs that time out have no collected files, so their file criteria are `unavailable`, never passed.
