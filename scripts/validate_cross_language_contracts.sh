#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
TS_DIR="$ROOT/contracts/conformance/typescript"
RUST_MANIFEST="$ROOT/contracts/conformance/rust/Cargo.toml"
CORE_MANIFEST="$ROOT/crates/evaluation-core/Cargo.toml"

for command_name in node npm cargo; do
  if ! command -v "$command_name" >/dev/null 2>&1; then
    printf 'ERROR: required command not found: %s\n' "$command_name" >&2
    exit 2
  fi
done

printf '\n== JSON Schema strict-keyword audit ==\n'
python3 "$ROOT/scripts/audit_schema_keyword_types.py"

printf '\n== TypeScript / Ajv conformance ==\n'
npm --prefix "$TS_DIR" ci
npm --prefix "$TS_DIR" test

printf '\n== Rust evaluation-core unit tests ==\n'
cargo test --manifest-path "$CORE_MANIFEST"

printf '\n== Rust / jsonschema conformance ==\n'
cargo run --manifest-path "$RUST_MANIFEST" -- "$ROOT"

printf '\nCross-language schema and domain-invariant conformance checks passed.\n'
