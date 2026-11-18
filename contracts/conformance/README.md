# Cross-Language Contract Conformance

This directory contains independent TypeScript/Node and Rust validators for the canonical JSON Schema Draft 2020-12 contracts.

Both validators consume the same repository-owned schema files and the same `contracts/fixtures/{valid,invalid}` corpus. Neither validator may fetch schemas over the network. All `$ref` targets must resolve from the checked-in schema registry.

## TypeScript / Node

Requirements: Node.js 20 or newer and npm.

```sh
cd contracts/conformance/typescript
npm install
npm test
```

The package uses the Draft 2020-12-specific Ajv implementation and explicitly registers `ajv-formats`. It runs a date-time negative probe before processing fixtures.

## Rust

Requirements: a current stable Rust toolchain with Cargo.

From the repository root:

```sh
cargo run --manifest-path contracts/conformance/rust/Cargo.toml -- .
```

The Rust harness uses `jsonschema` with Draft 2020-12 options, explicit format assertion, an in-memory registry containing every checked-in schema, and offline resolution. It must never fetch a missing `$ref` from the network.

## Unified command

From the repository root:

```sh
bash scripts/validate_cross_language_contracts.sh
```

This installs the TypeScript package dependencies and runs both validators. Cargo resolves Rust dependencies when first run.

## Expected parity

Both implementations should report identical counts for:

- Schemas discovered and compiled.
- Valid fixtures accepted.
- Invalid fixtures rejected.
- Format assertion enabled and negative date-time probe rejected.

A passing count comparison is necessary but not sufficient for full semantic parity. When a mismatch appears, keep the fixture and add a focused regression case. Do not weaken a schema or turn off a validator option merely to make counts match.

## Explicit current boundary

This iteration checks schema-level conformance only. The Python domain-invariant validator remains the reference implementation for cross-record rules such as causal graph integrity, intervention confirmation, metric arithmetic, comparison compatibility, and capability-passport denominators. Those domain invariants still need independent TypeScript and Rust implementations and shared cross-language fixtures before claiming domain-level parity.

The official JSON Schema Test Suite is a separate, language-agnostic project. This Workbench fixture corpus tests our own contract behavior; it does not replace the upstream conformance suite.
