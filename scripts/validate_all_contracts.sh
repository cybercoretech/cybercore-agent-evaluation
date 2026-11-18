#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
PYTHON_BIN="${PYTHON:-python3}"

cd "$ROOT"

echo "== Python / JSON Schema contracts =="
"$PYTHON_BIN" scripts/validate_contracts.py

echo
echo "== Python / domain-invariant reference =="
"$PYTHON_BIN" scripts/validate_domain_invariants.py

echo
echo "== TypeScript + Rust cross-language conformance =="
bash scripts/validate_cross_language_contracts.sh

echo
echo "All contract and domain-invariant checks passed."
