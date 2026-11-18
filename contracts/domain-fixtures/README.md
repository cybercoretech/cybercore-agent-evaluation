# Domain invariant fixtures

These fixtures exercise constraints that JSON Schema cannot fully enforce. `valid/` cases must pass and `invalid/` cases must be rejected by `scripts/validate_domain_invariants.py`. They are synthetic and do not establish cross-record database integrity.


The benchmark/metrics domain fixtures cover benchmark freeze/audit constraints, metric denominator arithmetic, retry-versus-trial accounting, one-population analysis scope, and paired/unpaired comparison consistency.


Additional benchmark/metrics positive and negative cases exercise frozen benchmark audit evidence, possible-contamination mitigation, metric target ranges, trial/attempt denominators, zero-observation rates, comparison scope resolution, conditional comparability, and confirmatory inference requirements.
