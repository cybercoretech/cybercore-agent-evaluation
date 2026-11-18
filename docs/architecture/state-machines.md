# Lifecycle and State Machines

**Version:** 0.1 — proposed  
**Status:** review before implementation

State dimensions must remain orthogonal. A successful process execution does not prove the agent succeeded; an agent-reported success does not prove independent verification; a passing result does not prove attribution; and none of those alone implies readiness.

## 1. Evaluation plan lifecycle

```text
DRAFT -> VALIDATING -> FROZEN -> SUPERSEDED
          |             |
          v             v
        INVALID       (immutable)
```

- `draft`: being authored; incomplete fields are allowed.
- `validating`: static and semantic checks are running.
- `frozen`: all required design fields and approvals are present; a stable revision/hash is issued.
- `invalid`: a contract, sampling, safety, or benchmark dependency is not acceptable.
- `superseded`: a newer plan revision replaces it for future use; historical evaluations still point to the original.

A frozen plan is not edited in place. Create a new revision.

## 2. Evaluation campaign lifecycle

```text
CREATED -> QUEUED -> PREPARING -> RUNNING -> FINALIZING -> COMPLETED
   |         |          |           |             |
   +---------+----------+-----------+-------------+----> CANCELLED
                         |           |             +----> PARTIAL
                         +-----------+------------------> FAILED
```

- `completed`: all required trial slots are resolved according to plan policy.
- `partial`: some trial slots completed, but the campaign ended before all required work resolved.
- `failed`: the campaign orchestration could not proceed or finalize correctly; this is not automatically an agent task failure.
- `cancelled`: explicitly stopped; record who/why and completed work.

Campaign progress is calculated from trial states. Never infer campaign result from one run.

## 3. Trial lifecycle

```text
PLANNED -> READY -> ASSIGNED -> EXECUTING -> RESOLVING -> RESOLVED
   |         |          |             |            |
   +---------+----------+-------------+------------+--> INVALIDATED
```

A trial is a planned sampling unit. It records task revision, condition, replicate, stratum, order/randomization, and analysis eligibility. A trial can be invalidated only with a reason and evidence. An infrastructure retry remains attached to the same trial unless the plan defines a new replicate.

## 4. Run execution status and run outcome are separate

### Execution status

`created | queued | starting | running | finalizing | finished | errored | timed_out | cancelled`

This describes the execution process. `errored`, `timed_out`, and `cancelled` do not automatically mean the agent failed the task; the trial outcome depends on the evaluation plan’s declared rules and independent evidence.

### Run outcome

`pending | pass | fail | inconclusive | not_scored | invalid`

This describes the evaluated result of the attempt. It is assigned only after required verification/analysis or an explicit reason for not scoring. Do not map execution status directly to this field.

### Verification state

`not_started | in_progress | verified_pass | verified_fail | mixed | inconclusive | unavailable`

Verification results are criterion-specific. The aggregate state must preserve mixed or unavailable evidence rather than force a binary outcome.

## 5. Benchmark task validity

`draft | valid | under_review | invalid | deprecated`

A task’s validity state is independent from run outcomes. If a benchmark defect is discovered after runs execute, preserve the affected runs and mark the benchmark/task revision under review or invalid; recompute affected reports with explicit revision history.

## 6. Attribution lifecycle

`uninvestigated | investigating | provisional | confirmed | refuted | unresolved`

- `confirmed` requires direct mechanistic evidence establishing the mechanism or a suitably controlled and replicated intervention supporting it.
- A single uncontrolled replay, temporal association, or LLM-generated diagnosis cannot confirm a cause.
- `unresolved` is a valid terminal disposition when evidence is missing, irrecoverable, or conflicting.
- Reopening an investigation creates a new revision or status event; it does not erase prior conclusions.

Keep `confidence` and `evidence_quality` as separate attributes. Qualitative confidence labels are not calibrated probabilities unless calibration has been demonstrated.

## 7. Intervention lifecycle

`draft | awaiting_approval | approved | scheduled | running | completed | rejected | rolled_back | abandoned`

A completed intervention can yield `supported | refuted | inconclusive | confounded` as its scientific result. Do not encode that result in the lifecycle status. Approval is a separate actor/action with a timestamp and scope.

## 8. Evaluation decision state

`ready | conditionally_ready | not_ready | insufficient_evidence | not_assessed`

A decision must always include a bounded workload and operating scope. `insufficient_evidence` is not the same as `not_ready`; it states that the available evidence cannot justify a readiness conclusion. Safety gates are non-compensatory: a good aggregate score must not override a prohibited action or failed mandatory gate.

## 9. Transition rules

- Every transition records actor, timestamp, reason, source revision, and relevant evidence.
- Only documented transitions are accepted; no arbitrary status mutation.
- Idempotent commands must return the original transition/result when replayed with the same idempotency key.
- Terminal states require explicit recovery/reopen paths; do not silently reset a record to an earlier state.
- A projection may be rebuilt from the authoritative record history and referenced execution evidence.
- Derived aggregate results include the analysis version and source record revisions.
