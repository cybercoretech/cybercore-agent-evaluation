# ADR-0016: Scheduler State Snapshots

- **Status:** Accepted (implemented in `evaluation-core::orchestration` and `evaluation-store::scheduler`)
- **Date:** 2026-11-19
- **Builds on:** ADR-0010 (durable scheduling), ADR-0011 (run records)

## Context

`DurableScheduler::acquire` rebuilt the coordinator by replaying the whole journal, so restart time grew linearly with history: one reserve and one complete entry per attempt, each carrying the runner result. `run_ready_trials` also re-read and re-parsed the entire journal on every start for crash catch-up, even when nothing was missing.

Measured on blackbox (release build, 5,000 trials, 10,000 entries with 4 KiB stdout each): a cold `acquire` took about **125 ms**.

## Decision

- **Runtime-only coordinator state.** `TrialCoordinator::export_state` serializes only runtime state: each trial's status, attempts and active request, plus the schedule fields used for checking. Trial definitions are left out and come from the schedule on restore; the active-run count and identity sets are recomputed.
- **Strict restore.** `TrialCoordinator::restore_state(schedule, state)` refuses any state that:
  - names a different evaluation, plan, `max_active_runs`, retry policy or trial set;
  - is internally inconsistent: an active request without an in-flight trial or the reverse, attempt numbers out of order, too many attempts, repeated identities, or too many active runs.
- **Consistent by construction.** Every `N` journal commands (default 1,000, `set_snapshot_interval`, 0 disables), the scheduler writes a snapshot **in the same transaction as journal entry `N`**. The coordinator already includes that command, so the snapshot is exactly the state after it. Only the latest snapshot is kept (`scheduler_snapshots`, migration 0003, plain SQLite).
- **Recovery with fallback.** `acquire` restores the latest snapshot if its state digest matches, its schedule digest matches the stored schedule, its position is within the journal, and `restore_state` accepts it. It then replays only the entries after it, still checking for gaps. Any failure falls back to a full replay, because the journal remains the source of truth. `RecoveryReport` gains `snapshot_seq` and `snapshot_rejected`.
- **Less re-reading.** In-flight attempts are found from the restored coordinator instead of the journal, and the schedule is parsed once per `acquire`.
- **Catch-up pre-check.** `record_completed_runs` first asks, using a new `record_exists`, which resolved attempts lack run records, and reads the journal only when some do.

## Consequences

- Measured on the same store (5,000 trials, 10,000 entries): a full replay takes about 92 ms after the schedule-parsing fix, and restoring from the snapshot about 66–74 ms. The snapshot removes the replay share, roughly 25–35 ms per 10,000 entries here. Restart time no longer grows with journal length, which matters for long evaluations with many retries. The remaining cost is proportional to the number of trials (schedule parse and coordinator construction).
- Snapshots are an optimisation, never a dependency. Deleting or corrupting them only costs time, and tests show the restored state is byte-identical to a full replay.
- The interval belongs to each scheduler instance. Commands journaled during `acquire` (closing interrupted attempts) use the default interval.
