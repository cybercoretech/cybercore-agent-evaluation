//! Several verifiers judging one evaluation (ADR-0018): for example exact
//! `checks` for files plus a model judge for rubric criteria.

use crate::recorder::{
    AttributedVerdict, CriterionVerdict, VerdictResult, VerificationInput, Verifier,
    VerifierIdentity, VerifierType,
};
use std::collections::BTreeMap;
use std::future::Future;
use std::pin::Pin;

/// Runs every sub-verifier that covers a trial and keeps each verdict's own
/// verifier identity, so a `verification_result` says which verifier judged
/// it. A criterion ID produced by more than one verifier for the same trial
/// becomes a single `unavailable` verdict (their records would collide, and
/// silently preferring one would hide the other). A trial no sub-verifier
/// covers gets one `unavailable` verdict, never a pass.
pub struct CompositeVerifier {
    verifiers: Vec<Box<dyn Verifier>>,
}

impl CompositeVerifier {
    pub fn new(verifiers: Vec<Box<dyn Verifier>>) -> Self {
        Self { verifiers }
    }

    fn covering(&self, trial_id: &str) -> Vec<&dyn Verifier> {
        self.verifiers
            .iter()
            .map(|v| v.as_ref())
            .filter(|v| v.covers(trial_id))
            .collect()
    }

    fn settle(&self, trial_id: &str, verdicts: Vec<AttributedVerdict>) -> Vec<AttributedVerdict> {
        if verdicts.is_empty() {
            return vec![(
                self.identity(),
                CriterionVerdict {
                    criterion_id: "no-verifier-configured".into(),
                    inspected_target: format!("trial:{trial_id}"),
                    method: "Look up the verifiers configured for this trial.".into(),
                    result: VerdictResult::Unavailable,
                    limitations: vec![format!("No verifier is configured for {trial_id}.")],
                    judge_rationale: None,
                    evidence_paths: Vec::new(),
                },
            )];
        }
        let mut by_criterion: BTreeMap<String, Vec<AttributedVerdict>> = BTreeMap::new();
        let mut order = Vec::new();
        for (identity, verdict) in verdicts {
            let key = verdict.criterion_id.clone();
            if !by_criterion.contains_key(&key) {
                order.push(key.clone());
            }
            by_criterion
                .entry(key)
                .or_default()
                .push((identity, verdict));
        }
        order
            .into_iter()
            .filter_map(|criterion| {
                let mut group = by_criterion.remove(&criterion)?;
                if group.len() == 1 {
                    return group.pop();
                }
                let producers: Vec<String> =
                    group.iter().map(|(id, _)| id.verifier_id.clone()).collect();
                Some((
                    self.identity(),
                    CriterionVerdict {
                        criterion_id: criterion.clone(),
                        inspected_target: format!("trial:{trial_id}"),
                        method: "Combine the verdicts of the configured verifiers.".into(),
                        result: VerdictResult::Unavailable,
                        limitations: vec![format!(
                            "Criterion {criterion} was judged by more than one verifier ({}); give each verifier its own criterion IDs.",
                            producers.join(", ")
                        )],
                        judge_rationale: None,
                        evidence_paths: Vec::new(),
                    },
                ))
            })
            .collect()
    }
}

impl Verifier for CompositeVerifier {
    fn identity(&self) -> VerifierIdentity {
        let ids: Vec<String> = self
            .verifiers
            .iter()
            .map(|v| v.identity().verifier_id)
            .collect();
        let mut verifier_id = format!("composite:{}", ids.join("+"));
        if verifier_id.len() > 128 {
            verifier_id.truncate(128);
        }
        VerifierIdentity {
            verifier_id,
            version: env!("CARGO_PKG_VERSION").into(),
            verifier_type: VerifierType::Hybrid,
        }
    }

    fn covers(&self, trial_id: &str) -> bool {
        self.verifiers.iter().any(|v| v.covers(trial_id))
    }

    fn verify(&self, input: &VerificationInput<'_>) -> Vec<CriterionVerdict> {
        self.verify_attributed(input)
            .into_iter()
            .map(|(_, verdict)| verdict)
            .collect()
    }

    fn verify_attributed(&self, input: &VerificationInput<'_>) -> Vec<AttributedVerdict> {
        let trial_id = &input.trial.trial_ref.record_id;
        let verdicts = self
            .covering(trial_id)
            .into_iter()
            .flat_map(|v| v.verify_attributed(input))
            .collect();
        self.settle(trial_id, verdicts)
    }

    fn verify_attributed_async<'a>(
        &'a self,
        input: &'a VerificationInput<'a>,
    ) -> Pin<Box<dyn Future<Output = Vec<AttributedVerdict>> + 'a>> {
        Box::pin(async move {
            let trial_id = &input.trial.trial_ref.record_id;
            let mut verdicts = Vec::new();
            for verifier in self.covering(trial_id) {
                verdicts.extend(verifier.verify_attributed_async(input).await);
            }
            self.settle(trial_id, verdicts)
        })
    }
}
