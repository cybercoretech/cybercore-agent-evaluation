//! Record validation against the embedded v0 JSON Schema contracts.
//!
//! The schemas are compiled into the binary (`include_str!`) and every `$ref`
//! resolves from that embedded registry only, so validation never touches the
//! network and cannot drift from the checked-in contracts of this build.

use crate::embedded_schemas::EMBEDDED_SCHEMAS;
use crate::error::{StoreError, StoreResult};
use jsonschema::{draft202012, Registry, Validator};
use serde_json::Value;
use std::collections::BTreeMap;

const DRAFT_2020_12: &str = "https://json-schema.org/draft/2020-12/schema";
const MAX_REPORTED_ERRORS: usize = 5;

/// Compiled per-record-type validators. Build once and reuse: compiling all
/// schemas is the expensive part, validating a record is cheap.
pub struct ContractValidator {
    by_record_type: BTreeMap<String, Validator>,
}

impl ContractValidator {
    /// Compiles the contracts embedded in this build.
    pub fn embedded() -> StoreResult<Self> {
        let mut parsed = Vec::with_capacity(EMBEDDED_SCHEMAS.len());
        for (path, text) in EMBEDDED_SCHEMAS {
            let schema: Value = serde_json::from_str(text)
                .map_err(|error| StoreError::Contract(format!("{path}: {error}")))?;
            let id = schema
                .get("$id")
                .and_then(Value::as_str)
                .ok_or_else(|| StoreError::Contract(format!("{path}: missing $id")))?
                .to_string();
            if schema.get("$schema").and_then(Value::as_str) != Some(DRAFT_2020_12) {
                return Err(StoreError::Contract(format!(
                    "{path}: not a Draft 2020-12 schema"
                )));
            }
            parsed.push((path, id, schema));
        }

        let mut registry = Registry::new();
        for (path, id, schema) in &parsed {
            registry = registry
                .add(id.clone(), schema.clone())
                .map_err(|error| StoreError::Contract(format!("{path}: {error}")))?;
        }
        let registry = registry
            .prepare()
            .map_err(|error| StoreError::Contract(error.to_string()))?;

        let mut by_record_type = BTreeMap::new();
        for (path, _, schema) in &parsed {
            let Some(record_type) = declared_record_type(schema) else {
                continue; // component schema, validated through $ref
            };
            let validator = draft202012::options()
                .with_registry(&registry)
                .offline()
                .should_validate_formats(true)
                .build(schema)
                .map_err(|error| StoreError::Contract(format!("{path}: {error}")))?;
            if by_record_type
                .insert(record_type.clone(), validator)
                .is_some()
            {
                return Err(StoreError::Contract(format!(
                    "record_type {record_type} is declared by more than one schema"
                )));
            }
        }
        Ok(Self { by_record_type })
    }

    /// Record types this build can validate and store.
    pub fn record_types(&self) -> impl Iterator<Item = &str> {
        self.by_record_type.keys().map(String::as_str)
    }

    /// Validates `record` against the schema for its `record_type`.
    pub fn validate(&self, record: &Value) -> StoreResult<()> {
        let record_type = record
            .get("record_type")
            .and_then(Value::as_str)
            .ok_or_else(|| StoreError::InvalidRecord("record_type must be a string".into()))?;
        let validator = self.by_record_type.get(record_type).ok_or_else(|| {
            StoreError::InvalidRecord(format!("unknown record_type {record_type:?}"))
        })?;
        let errors: Vec<String> = validator
            .iter_errors(record)
            .take(MAX_REPORTED_ERRORS)
            .map(|error| format!("{} at {}", error, error.instance_path()))
            .collect();
        if errors.is_empty() {
            Ok(())
        } else {
            Err(StoreError::InvalidRecord(format!(
                "{record_type} violates its schema: {}",
                errors.join("; ")
            )))
        }
    }
}

/// The `record_type` constant a persisted-record schema declares, if any.
fn declared_record_type(schema: &Value) -> Option<String> {
    let parts = std::iter::once(schema).chain(
        schema
            .get("allOf")
            .and_then(Value::as_array)
            .into_iter()
            .flatten(),
    );
    for part in parts {
        if let Some(constant) = part
            .pointer("/properties/record_type/const")
            .and_then(Value::as_str)
        {
            return Some(constant.to_string());
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn embeds_every_checked_in_schema() {
        let on_disk = std::fs::read_dir(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../contracts/schemas/v0"
        ))
        .unwrap()
        .flat_map(|dir| std::fs::read_dir(dir.unwrap().path()).unwrap())
        .filter(|entry| {
            entry
                .as_ref()
                .unwrap()
                .file_name()
                .to_string_lossy()
                .ends_with(".schema.json")
        })
        .count();
        assert_eq!(EMBEDDED_SCHEMAS.len(), on_disk);
    }

    #[test]
    fn compiles_one_validator_per_record_type() {
        let validator = ContractValidator::embedded().unwrap();
        assert_eq!(validator.record_types().count(), 19);
        assert!(validator.record_types().any(|t| t == "evidence_artifact"));
    }
}
