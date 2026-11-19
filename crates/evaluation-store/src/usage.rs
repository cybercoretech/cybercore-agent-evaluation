//! Agent-reported usage and declared prices (ADR-0022).
//!
//! The workbench cannot see inside an agent's encrypted model-API traffic, so
//! token counts come from the agent itself: it may write
//! [`USAGE_FILE`] into its collected outputs. The report is recorded on the
//! run as `agent_report.reported_usage` with `source: agent_self_reported`,
//! never as a measurement (`resource_usage` holds only what the workbench
//! measured). Cost is derived from the reported counts and the prices the
//! evaluation file declared.
//!
//! An invalid report is recorded as `invalid` with a reason written by the
//! workbench; agent-written text is never copied into the reason.

use crate::canonical::{canonical_json, sha256_hex};
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};

/// The usage report's path inside the collected outputs.
pub const USAGE_FILE: &str = "cybercore-usage.json";
/// The only usage report format this revision reads.
pub const USAGE_FORMAT: &str = "cybercore-usage/v0";
const MAX_USAGE_FILE_BYTES: usize = 64 * 1024;
const MAX_COUNT: u64 = 1_000_000_000_000;
const MAX_MODELS: usize = 32;
/// Highest accepted price per million tokens, keeping costs exact in `f64`.
pub const MAX_PRICE_PER_MILLION: f64 = 10_000.0;

/// Price of one model, in currency units per million tokens.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ModelPrice {
    pub input_per_million_tokens: f64,
    pub output_per_million_tokens: f64,
}

/// Declared prices for an evaluation, in one currency.
#[derive(Debug, Clone, PartialEq)]
pub struct Pricing {
    currency: String,
    models: BTreeMap<String, ModelPrice>,
}

impl Pricing {
    /// Validates the currency (ISO 4217 style, three capitals), model IDs and
    /// prices (finite, from 0 to [`MAX_PRICE_PER_MILLION`]).
    pub fn new(currency: &str, models: BTreeMap<String, ModelPrice>) -> Result<Self, String> {
        if currency.len() != 3 || !currency.bytes().all(|b| b.is_ascii_uppercase()) {
            return Err(format!(
                "currency must be three capital letters (for example USD), not {currency:?}"
            ));
        }
        if models.is_empty() {
            return Err("pricing must list at least one model".into());
        }
        if models.len() > MAX_MODELS {
            return Err(format!("pricing lists more than {MAX_MODELS} models"));
        }
        for (model, price) in &models {
            if !valid_model_id(model) {
                return Err(format!("{model:?} is not a valid model ID"));
            }
            for (name, value) in [
                ("input_per_million_tokens", price.input_per_million_tokens),
                ("output_per_million_tokens", price.output_per_million_tokens),
            ] {
                if !value.is_finite() || !(0.0..=MAX_PRICE_PER_MILLION).contains(&value) {
                    return Err(format!(
                        "{model}.{name} must be between 0 and {MAX_PRICE_PER_MILLION}"
                    ));
                }
            }
        }
        Ok(Self {
            currency: currency.to_string(),
            models,
        })
    }

    pub fn currency(&self) -> &str {
        &self.currency
    }

    /// SHA-256 of the canonical price table, recorded with every cost so
    /// costs computed under different prices can be told apart.
    pub fn digest(&self) -> String {
        let models: BTreeMap<&str, Value> = self
            .models
            .iter()
            .map(|(model, price)| {
                (
                    model.as_str(),
                    json!({
                        "input_per_million_tokens": price.input_per_million_tokens,
                        "output_per_million_tokens": price.output_per_million_tokens,
                    }),
                )
            })
            .collect();
        sha256_hex(canonical_json(&json!({"currency": self.currency, "models": models})).as_bytes())
    }

    /// Cost in millionths of the currency unit. Tokens × price per million
    /// tokens is already in micro-units, so no scaling is needed.
    fn cost_microunits(&self, model: &str, input: u64, output: u64) -> Option<u64> {
        let price = self.models.get(model)?;
        let cost = input as f64 * price.input_per_million_tokens
            + output as f64 * price.output_per_million_tokens;
        Some(cost.round() as u64)
    }
}

fn valid_model_id(id: &str) -> bool {
    let bytes = id.as_bytes();
    !bytes.is_empty()
        && bytes.len() <= 128
        && bytes[0].is_ascii_alphanumeric()
        && bytes
            .iter()
            .all(|b| b.is_ascii_alphanumeric() || b"._:/@-".contains(b))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct UsageFile {
    format: String,
    models: Vec<ModelUsage>,
    #[serde(default)]
    tool_calls: Option<u64>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ModelUsage {
    model_id: String,
    input_tokens: u64,
    output_tokens: u64,
}

fn invalid(reason: &str) -> Value {
    json!({
        "source": "agent_self_reported",
        "report_status": "invalid",
        "invalid_reason": reason,
    })
}

/// The `agent_report.reported_usage` value for a usage file's bytes, with a
/// cost when `pricing` prices every reported model.
pub fn reported_usage(bytes: &[u8], pricing: Option<&Pricing>) -> Value {
    if bytes.len() > MAX_USAGE_FILE_BYTES {
        return invalid("the usage file is larger than 64 KiB");
    }
    let Ok(file) = serde_json::from_slice::<UsageFile>(bytes) else {
        return invalid(
            "the usage file is not JSON in the cybercore-usage/v0 format (format, models, optional tool_calls; no other fields)",
        );
    };
    if file.format != USAGE_FORMAT {
        return invalid("the usage file's format is not cybercore-usage/v0");
    }
    if file.models.is_empty() || file.models.len() > MAX_MODELS {
        return invalid("the usage file must list between 1 and 32 models");
    }
    let mut seen = BTreeSet::new();
    for model in &file.models {
        if !valid_model_id(&model.model_id) {
            return invalid("the usage file lists an invalid model ID");
        }
        if !seen.insert(model.model_id.as_str()) {
            return invalid("the usage file lists a model more than once");
        }
        if model.input_tokens > MAX_COUNT || model.output_tokens > MAX_COUNT {
            return invalid("the usage file reports a count above 10^12");
        }
    }
    if file.tool_calls.is_some_and(|n| n > MAX_COUNT) {
        return invalid("the usage file reports a count above 10^12");
    }
    let input: u64 = file.models.iter().map(|m| m.input_tokens).sum();
    let output: u64 = file.models.iter().map(|m| m.output_tokens).sum();
    if input > MAX_COUNT || output > MAX_COUNT {
        return invalid("the usage file reports a count above 10^12");
    }
    let models: Vec<Value> = file
        .models
        .iter()
        .map(|m| {
            json!({"model_id": m.model_id, "input_tokens": m.input_tokens, "output_tokens": m.output_tokens})
        })
        .collect();
    let mut report = json!({
        "source": "agent_self_reported",
        "report_status": "reported",
        "input_tokens": input,
        "output_tokens": output,
        "models": models,
    });
    let Some(map) = report.as_object_mut() else {
        return report;
    };
    if let Some(calls) = file.tool_calls {
        map.insert("tool_calls".into(), json!(calls));
    }
    if let Some(pricing) = pricing {
        let unpriced: Vec<&str> = file
            .models
            .iter()
            .filter(|m| !pricing.models.contains_key(&m.model_id))
            .map(|m| m.model_id.as_str())
            .collect();
        if unpriced.is_empty() {
            let cost: u64 = file
                .models
                .iter()
                .filter_map(|m| {
                    pricing.cost_microunits(&m.model_id, m.input_tokens, m.output_tokens)
                })
                .sum();
            map.insert("cost_microunits".into(), json!(cost));
            map.insert("currency".into(), json!(pricing.currency));
            map.insert("pricing_digest".into(), json!(pricing.digest()));
        } else {
            map.insert("unpriced_model_ids".into(), json!(unpriced));
        }
    }
    report
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pricing() -> Pricing {
        Pricing::new(
            "USD",
            BTreeMap::from([(
                "model-a".to_string(),
                ModelPrice {
                    input_per_million_tokens: 3.0,
                    output_per_million_tokens: 15.0,
                },
            )]),
        )
        .unwrap()
    }

    #[test]
    fn a_priced_report_gets_a_cost() {
        let file = br#"{"format":"cybercore-usage/v0","models":[{"model_id":"model-a","input_tokens":10000,"output_tokens":2000}],"tool_calls":4}"#;
        let report = reported_usage(file, Some(&pricing()));
        assert_eq!(report["report_status"], "reported");
        assert_eq!(report["input_tokens"], 10000);
        assert_eq!(report["tool_calls"], 4);
        // 10 000 × 3 + 2 000 × 15 = 60 000 micro-dollars = $0.06.
        assert_eq!(report["cost_microunits"], 60000);
        assert_eq!(report["currency"], "USD");
        assert_eq!(report["pricing_digest"], json!(pricing().digest()));
    }

    #[test]
    fn an_unpriced_model_leaves_the_cost_out() {
        let file = br#"{"format":"cybercore-usage/v0","models":[{"model_id":"model-a","input_tokens":1,"output_tokens":1},{"model_id":"model-b","input_tokens":5,"output_tokens":5}]}"#;
        let report = reported_usage(file, Some(&pricing()));
        assert_eq!(report["report_status"], "reported");
        assert_eq!(report["input_tokens"], 6);
        assert!(report.get("cost_microunits").is_none());
        assert_eq!(report["unpriced_model_ids"], json!(["model-b"]));
        let unpriced = reported_usage(file, None);
        assert!(unpriced.get("unpriced_model_ids").is_none());
    }

    #[test]
    fn invalid_reports_never_echo_agent_text() {
        for file in [
            &b"not json SECRET"[..],
            br#"{"format":"cybercore-usage/v0","models":[],"SECRET":1}"#,
            br#"{"format":"other","models":[{"model_id":"m","input_tokens":1,"output_tokens":1}]}"#,
            br#"{"format":"cybercore-usage/v0","models":[{"model_id":"SECRET value","input_tokens":1,"output_tokens":1}]}"#,
            br#"{"format":"cybercore-usage/v0","models":[{"model_id":"m","input_tokens":-1,"output_tokens":1}]}"#,
            br#"{"format":"cybercore-usage/v0","models":[{"model_id":"m","input_tokens":1,"output_tokens":1},{"model_id":"m","input_tokens":1,"output_tokens":1}]}"#,
            br#"{"format":"cybercore-usage/v0","models":[{"model_id":"m","input_tokens":1000000000001,"output_tokens":1}]}"#,
        ] {
            let report = reported_usage(file, Some(&pricing()));
            assert_eq!(report["report_status"], "invalid", "{report}");
            assert!(!report.to_string().contains("SECRET"));
            assert!(report.get("input_tokens").is_none());
        }
    }

    #[test]
    fn pricing_is_validated() {
        let price = |input| {
            BTreeMap::from([(
                "m".to_string(),
                ModelPrice {
                    input_per_million_tokens: input,
                    output_per_million_tokens: 1.0,
                },
            )])
        };
        assert!(Pricing::new("usd", price(1.0)).is_err());
        assert!(Pricing::new("USD", price(-1.0)).is_err());
        assert!(Pricing::new("USD", price(f64::NAN)).is_err());
        assert!(Pricing::new("USD", price(20_000.0)).is_err());
        assert!(Pricing::new("USD", BTreeMap::new()).is_err());
        assert!(Pricing::new("EUR", price(0.0)).is_ok());
    }
}
