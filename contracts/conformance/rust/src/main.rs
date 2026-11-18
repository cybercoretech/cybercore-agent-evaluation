use anyhow::{bail, Context, Result};
use jsonschema::{draft202012, Registry};
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};
use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use walkdir::WalkDir;

const DRAFT_2020_12: &str = "https://json-schema.org/draft/2020-12/schema";

#[derive(Clone)]
struct SchemaEntry {
    path: PathBuf,
    id: String,
    schema: Value,
}

fn read_json(path: &Path) -> Result<Value> {
    let text = fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    serde_json::from_str(&text).with_context(|| format!("parsing JSON in {}", path.display()))
}

fn collect_schema_paths(root: &Path) -> Result<Vec<PathBuf>> {
    let mut paths = WalkDir::new(root)
        .into_iter()
        .filter_map(|entry| entry.ok())
        .filter(|entry| entry.file_type().is_file())
        .filter(|entry| {
            entry
                .file_name()
                .to_string_lossy()
                .ends_with(".schema.json")
        })
        .map(|entry| entry.into_path())
        .collect::<Vec<_>>();
    paths.sort();
    if paths.is_empty() {
        bail!("no schemas found under {}", root.display());
    }
    Ok(paths)
}

fn collect_record_type_constants(value: &Value, found: &mut BTreeSet<String>) {
    match value {
        Value::Array(items) => {
            for item in items {
                collect_record_type_constants(item, found);
            }
        }
        Value::Object(object) => {
            if let Some(record_type) = object
                .get("properties")
                .and_then(|v| v.get("record_type"))
                .and_then(|v| v.get("const"))
                .and_then(Value::as_str)
            {
                found.insert(record_type.to_string());
            }
            for child in object.values() {
                collect_record_type_constants(child, found);
            }
        }
        _ => {}
    }
}

fn collect_fixture_paths(root: &Path) -> Result<Vec<PathBuf>> {
    let mut paths = fs::read_dir(root)
        .with_context(|| format!("reading fixture directory {}", root.display()))?
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.path())
        .filter(|path| path.is_file() && path.extension().is_some_and(|ext| ext == "json"))
        .collect::<Vec<_>>();
    paths.sort();
    if paths.is_empty() {
        bail!("no JSON fixtures found in {}", root.display());
    }
    Ok(paths)
}

fn main() -> Result<()> {
    let repository_root = match env::args().nth(1) {
        Some(root) => PathBuf::from(root).canonicalize()?,
        None => Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../..")
            .canonicalize()?,
    };
    let schema_root = repository_root.join("contracts/schemas/v0");
    let fixture_root = repository_root.join("contracts/fixtures");

    let mut schemas = Vec::new();
    let mut record_type_to_schema: BTreeMap<String, usize> = BTreeMap::new();
    let mut ids = BTreeSet::new();

    for schema_path in collect_schema_paths(&schema_root)? {
        let schema = read_json(&schema_path)?;
        let id = schema
            .get("$id")
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
            .context("schema is missing a non-empty string $id")?
            .to_string();
        if !ids.insert(id.clone()) {
            bail!("duplicate schema $id: {id}");
        }
        let dialect = schema.get("$schema").and_then(Value::as_str);
        if dialect != Some(DRAFT_2020_12) {
            bail!(
                "unexpected JSON Schema dialect in {}: {dialect:?}",
                schema_path.display()
            );
        }

        let index = schemas.len();
        let mut record_types = BTreeSet::new();
        collect_record_type_constants(&schema, &mut record_types);
        for record_type in record_types {
            if record_type_to_schema
                .insert(record_type.clone(), index)
                .is_some()
            {
                bail!("more than one persisted-record schema declares record_type={record_type}");
            }
        }
        schemas.push(SchemaEntry {
            path: schema_path,
            id,
            schema,
        });
    }

    // Resolve every $ref from the checked-in registry only. Offline mode below
    // makes a missing resource an error instead of silently fetching GitHub.
    let mut registry = Registry::new();
    for entry in &schemas {
        registry = registry.add(entry.id.clone(), entry.schema.clone())?;
    }
    let registry = registry.prepare()?;

    // Compile every schema before validating fixtures. This catches unresolved
    // references even when no fixture happens to exercise the affected branch.
    for entry in &schemas {
        draft202012::options()
            .with_registry(&registry)
            .offline()
            .should_validate_formats(true)
            .build(&entry.schema)
            .with_context(|| format!("compiling schema {}", entry.path.display()))?;
    }

    let probe_schema = json!({
        "$schema": DRAFT_2020_12,
        "type": "string",
        "format": "date-time"
    });
    let probe_validator = draft202012::options()
        .offline()
        .should_validate_formats(true)
        .build(&probe_schema)?;
    if probe_validator.is_valid(&json!("not-a-timestamp")) {
        bail!("date-time format checker accepted a deliberately invalid timestamp");
    }

    fn validate_fixture_directory(
        directory: &Path,
        expected_valid: bool,
        schemas: &[SchemaEntry],
        record_type_to_schema: &BTreeMap<String, usize>,
        registry: &Registry,
    ) -> Result<usize> {
        let paths = collect_fixture_paths(directory)?;
        for fixture_path in &paths {
            let record = read_json(fixture_path)?;
            let record_type = record
                .get("record_type")
                .and_then(Value::as_str)
                .with_context(|| {
                    format!(
                        "fixture lacks string record_type: {}",
                        fixture_path.display()
                    )
                })?;
            let index = record_type_to_schema.get(record_type).with_context(|| {
                format!(
                    "no schema declares record_type={record_type} for {}",
                    fixture_path.display()
                )
            })?;
            let entry = &schemas[*index];
            let validator = draft202012::options()
                .with_registry(registry)
                .offline()
                .should_validate_formats(true)
                .build(&entry.schema)
                .with_context(|| format!("building validator for {}", entry.path.display()))?;
            let passed = validator.is_valid(&record);
            if passed != expected_valid {
                let relative_fixture = fixture_path.display();
                let relative_schema = entry.path.display();
                bail!(
                    "{} unexpectedly {} schema {}",
                    relative_fixture,
                    if passed { "passed" } else { "failed" },
                    relative_schema
                );
            }
        }
        Ok(paths.len())
    }

    let valid_count = validate_fixture_directory(
        &fixture_root.join("valid"),
        true,
        &schemas,
        &record_type_to_schema,
        &registry,
    )?;
    let invalid_count = validate_fixture_directory(
        &fixture_root.join("invalid"),
        false,
        &schemas,
        &record_type_to_schema,
        &registry,
    )?;

    println!(
        "Rust jsonschema Draft 2020-12: compiled {} schemas; {} record types mapped.",
        schemas.len(),
        record_type_to_schema.len()
    );
    println!("Format assertions active: date-time; invalid-date-time probe rejected.");
    println!(
        "PASS: {valid_count} valid fixtures accepted; {invalid_count} invalid fixtures rejected."
    );

    let (domain_valid_count, domain_invalid_count) =
        cybercore_evaluation_core::validate_domain_fixtures(&repository_root)?;
    println!(
        "Rust domain invariants: PASS: {domain_valid_count} domain-valid fixtures; {domain_invalid_count} domain-invalid fixtures rejected."
    );
    Ok(())
}
