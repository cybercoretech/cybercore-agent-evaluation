#!/usr/bin/env python3
"""Validate Workbench Draft 2020-12 schemas and positive/negative fixtures.

Install the required validators with:
    python3 -m pip install -r requirements-contracts.txt

Run from the repository root:
    python3 scripts/validate_contracts.py

The validator fails closed when a format keyword used by the schemas has no
installed checker. This avoids JSON Schema's permitted behavior of treating
an unsupported format as an annotation and silently accepting its value.
"""
from __future__ import annotations

import json
import sys
from pathlib import Path
from typing import Any, Iterable

try:
    from jsonschema import Draft202012Validator, FormatChecker
    from jsonschema.exceptions import FormatError
    from referencing import Registry, Resource
except ImportError as exc:
    print(
        "Missing dependency. Install with: "
        "python3 -m pip install -r requirements-contracts.txt",
        file=sys.stderr,
    )
    raise SystemExit(2) from exc

ROOT = Path(__file__).resolve().parents[1]
SCHEMAS = ROOT / "contracts" / "schemas" / "v0"
FIXTURES = ROOT / "contracts" / "fixtures"
INSTALL_HINT = "python3 -m pip install -r requirements-contracts.txt"


def load_json(path: Path) -> Any:
    try:
        return json.loads(path.read_text(encoding="utf-8"))
    except Exception as exc:
        raise ValueError(f"{path.relative_to(ROOT)}: invalid JSON: {exc}") from exc


def part_list(schema: dict[str, Any]) -> Iterable[dict[str, Any]]:
    yield schema
    for part in schema.get("allOf", []):
        if isinstance(part, dict):
            yield part


def collect_formats(value: Any) -> set[str]:
    """Collect every JSON Schema `format` used at any nesting depth."""
    found: set[str] = set()
    if isinstance(value, dict):
        fmt = value.get("format")
        if isinstance(fmt, str):
            found.add(fmt)
        for child in value.values():
            found.update(collect_formats(child))
    elif isinstance(value, list):
        for child in value:
            found.update(collect_formats(child))
    return found


def require_format_support(
    schemas: dict[str, tuple[Path, dict[str, Any]]],
    checker: FormatChecker,
) -> set[str]:
    required = set().union(*(collect_formats(schema) for _, schema in schemas.values()))
    missing = sorted(required - set(checker.checkers))
    if missing:
        raise RuntimeError(
            "Required JSON Schema format checker(s) are unavailable: "
            f"{', '.join(missing)}. The schemas use these formats as validation "
            "constraints, so continuing would silently weaken validation. "
            f"Install the supported optional dependencies with: {INSTALL_HINT}"
        )

    # A checker may be registered only when its optional dependency is present.
    # Confirm the date-time checker actively rejects a known-invalid value.
    if "date-time" in required:
        try:
            checker.check("not-a-timestamp", "date-time")
        except FormatError:
            pass
        else:
            raise RuntimeError(
                "The installed date-time format checker did not reject an invalid "
                f"timestamp. Reinstall the contract dependencies with: {INSTALL_HINT}"
            )
    return required


def main() -> int:
    schema_paths = sorted(SCHEMAS.rglob("*.schema.json"))
    if not schema_paths:
        print("No schemas found.", file=sys.stderr)
        return 1

    schemas: dict[str, tuple[Path, dict[str, Any]]] = {}
    registry = Registry()
    for path in schema_paths:
        schema = load_json(path)
        Draft202012Validator.check_schema(schema)
        schema_id = schema.get("$id")
        if not schema_id:
            raise ValueError(f"{path.relative_to(ROOT)}: missing $id")
        if schema_id in schemas:
            raise ValueError(f"duplicate schema $id: {schema_id}")
        schemas[schema_id] = (path, schema)
        registry = registry.with_resource(schema_id, Resource.from_contents(schema))

    format_checker = FormatChecker()
    required_formats = require_format_support(schemas, format_checker)

    valid_paths = sorted((FIXTURES / "valid").glob("*.json"))
    invalid_paths = sorted((FIXTURES / "invalid").glob("*.json"))
    if not valid_paths or not invalid_paths:
        raise ValueError("Both valid and invalid fixture directories must contain JSON files.")

    def validate_fixture(path: Path, should_pass: bool) -> None:
        record = load_json(path)
        record_type = record.get("record_type")
        matches = [
            (p, s)
            for p, s in schemas.values()
            if any(
                part.get("properties", {}).get("record_type", {}).get("const") == record_type
                for part in part_list(s)
            )
        ]
        if not matches:
            raise ValueError(
                f"{path.relative_to(ROOT)}: no schema registered for record_type={record_type!r}"
            )
        record_matches = [
            (p, s) for p, s in matches
            if p != SCHEMAS / "common" / "record-envelope.schema.json"
        ]
        if len(record_matches) != 1:
            raise ValueError(
                f"{path.relative_to(ROOT)}: expected exactly one record schema "
                f"for {record_type!r}; got {len(record_matches)}"
            )
        schema_path, schema = record_matches[0]
        validator = Draft202012Validator(
            schema,
            registry=registry,
            format_checker=format_checker,
        )
        errors = sorted(
            validator.iter_errors(record),
            key=lambda error: list(map(str, error.absolute_path)),
        )
        if should_pass and errors:
            details = "; ".join(
                f"/{'/'.join(map(str, error.absolute_path))}: {error.message}"
                for error in errors[:4]
            )
            raise ValueError(
                f"{path.relative_to(ROOT)} unexpectedly failed "
                f"{schema_path.relative_to(ROOT)}: {details}"
            )
        if not should_pass and not errors:
            raise ValueError(
                f"{path.relative_to(ROOT)} unexpectedly passed "
                f"{schema_path.relative_to(ROOT)}"
            )

    for path in valid_paths:
        validate_fixture(path, True)
    for path in invalid_paths:
        validate_fixture(path, False)

    formats_display = ", ".join(sorted(required_formats)) or "none"
    print(f"Validated {len(schema_paths)} Draft 2020-12 schemas.")
    print(f"Format assertions active: {formats_display}.")
    print(
        f"PASS: {len(valid_paths)} valid fixtures; "
        f"{len(invalid_paths)} invalid fixtures rejected."
    )
    print("Note: cross-record and domain invariants require separate service-level tests.")
    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except (RuntimeError, ValueError) as exc:
        print(f"ERROR: {exc}", file=sys.stderr)
        raise SystemExit(1) from exc
