# ADR-0028: Readiness Decisions and Capability Passports

- **Status:** Accepted (implemented in the v0 `evaluation_plan` contract, `evaluation-store::analysis` (`readiness`) and `workbench passport` / `workbench report`)
- **Date:** 2026-11-19
- **Builds on:** ADR-0021 (frozen plans), ADR-0022 (speed and cost), ADR-0027 (failure records)

## Context

The contracts define a readiness `decision_record` (pass/fail gates, an outcome and a reviewer) and a `capability_passport` that must rest on such a decision. The methodology treats a passport as a version-specific, workload-bounded summary of evidence, not a permanent certificate. Nothing produced either record. The owner chose to have **gates declared in advance** rather than thresholds given on the command line, so nobody can set the bar after seeing the results.

## Decision

- **Contract (additive, v0):** the optional `evaluation_plan.analysis_plan.readiness` field:
  - holds `workload`, `autonomy_level`, `permitted_effects`, `excluded_effects`, `valid_for_days` (1–365) and 1–32 `gates`;
  - each gate has `gate_id`, `gate_category` and `mandatory`, plus exactly one form: `metric` with exactly one of `at_least` or `at_most`, or `criteria` with `max_failures`;
  - a valid fixture uses it, and an invalid fixture (a gate with both forms) is rejected by the Python, TypeScript and Rust suites;
  - readable descriptions also go into `decision_gates`.
- **Declaring gates.** Evaluation files and suites take a `readiness` section, validated as follows:
  - workload length, autonomy level, days, number of gates and unique gate IDs;
  - exactly one gate form, a known metric, one threshold, and pass-rate thresholds in [0, 1];
  - criteria that some trial's verifier checks;
  - one system fingerprint.

  It's frozen in the plan before the first trial runs; readiness alone is enough to freeze a plan, and plans frozen without it keep their content digest. Suites copy it into every agent's evaluation. The dry run lists the gates.
- **Evaluating gates.** `workbench passport EVAL --reviewer NAME` reads only the frozen gates:
  - **Metric gates** read the summary values: the pass rate and its lower bound, median and p90 wall time, mean tokens, and mean cost in currency units. If the underlying result isn't `computed`, the gate is inconclusive.
  - **Criteria gates** count verified failures of the named criteria from the failure analysis. Over `max_failures` fails the gate. A "no verdict" for those criteria, or a criterion judged on fewer trials than have an outcome, makes the gate inconclusive.
- **The outcome:**
  - **insufficient evidence** if the plan was frozen after the first result or the pass rate isn't `computed`;
  - otherwise **rejected** if a mandatory gate fails;
  - **insufficient evidence** if a mandatory gate is inconclusive;
  - **conditionally approved** if only optional gates didn't pass;
  - **approved** otherwise.
- **Records.** The IDs `DEC-` and `PASSPORT-` derive from the results, the frozen plan digest and the reviewer, so issuing again returns the existing passport.
  - **Evidence.** Every gate cites evidence: a criteria gate its criteria's verification results (the failing ones when it failed), a metric gate every latest-attempt verification result, falling back to terminal run events.
  - **The decision** records `decision_type: readiness`, the outcome, the scope (fingerprint, workload, frozen operating conditions, autonomy), gate results, evidence, conditions for optional gates that didn't pass, the reason and the reviewer.
  - **The passport** is **active** only when approved, with `issued_at`, `expires_at` and `review_by` set `valid_for_days` ahead. Otherwise it's a **draft**. It holds:
    - the scope, including task families and permitted and excluded effects;
    - trial counts;
    - one capability dimension per task family, with a Wilson interval and a note that repeated trials aren't independent;
    - links to the failure records;
    - limitations;
    - permitted-use constraints (this fingerprint, these task families, this autonomy level, any conditions).
- **CLI output:** `passport` prints the gates, the outcome with reasons, the passport and decision IDs, capability per family and the failure count; `--json` gives the records. `workbench report` gains a **Readiness** section with each evaluation's newest passport.

## Consequences

- An agent can now be approved for a stated workload by rules fixed before it ran, with a named reviewer, an expiry and links to its known failures. An agent that fails a mandatory gate gets a rejected draft explaining why.
- **The passport is only as good as the gates and the evidence.** Token and cost gates rest on self-reported usage, and the passport's limitations say so.
- **Not covered:**
  - revoking, expiring or reviewing passports later (the status is fixed at issue);
  - combining several evaluations into one passport;
  - safety gates beyond the declared criteria;
  - the reviewer's identity isn't verified (it's a recorded name).
