#!/usr/bin/env python3
"""Audit Draft 2020-12 schema scopes for Ajv strictTypes/strictRequired readiness.

This is a static preflight, not a replacement for running Ajv itself. It checks
that type-specific keywords have a matching explicit type in the same schema
object and that required property names are declared in that same schema scope.
"""
from __future__ import annotations

import json
import sys
from pathlib import Path
from typing import Any

ROOT = Path(__file__).resolve().parents[1]
SCHEMA_ROOT = ROOT / "contracts" / "schemas" / "v0"
DRAFT = "https://json-schema.org/draft/2020-12/schema"

KEYWORD_TYPES: dict[str, set[str]] = {
    "object": {
        "properties", "patternProperties", "additionalProperties", "propertyNames",
        "required", "minProperties", "maxProperties", "dependentRequired",
        "dependentSchemas", "dependencies", "unevaluatedProperties",
    },
    "array": {
        "items", "prefixItems", "additionalItems", "contains", "minContains",
        "maxContains", "minItems", "maxItems", "uniqueItems", "unevaluatedItems",
    },
    "string": {"minLength", "maxLength", "pattern"},
    "number": {"minimum", "maximum", "exclusiveMinimum", "exclusiveMaximum", "multipleOf"},
}

# These keywords contain child schemas. Map containers are traversed by value;
# the property-name mapping itself is not interpreted as a schema object.
SCHEMA_MAP_KEYWORDS = ("properties", "patternProperties", "$defs", "definitions", "dependentSchemas")
SINGLE_SCHEMA_KEYWORDS = (
    "items", "additionalItems", "additionalProperties", "unevaluatedProperties",
    "contains", "propertyNames", "not", "if", "then", "else",
    "unevaluatedItems", "contentSchema",
)
SCHEMA_ARRAY_KEYWORDS = ("allOf", "anyOf", "oneOf", "prefixItems")


def type_matches(declared: Any, expected: str) -> bool:
    values = set(declared if isinstance(declared, list) else [declared] if isinstance(declared, str) else [])
    return expected in values or (expected == "number" and "integer" in values)


def audit_schema(schema: Any, location: str, problems: list[str]) -> None:
    if not isinstance(schema, dict):
        return

    keys = set(schema)
    declared_type = schema.get("type")
    if isinstance(declared_type, list):
        problems.append(
            f"{location}: union-valued type {declared_type!r} is not permitted by the Ajv strict profile; "
            "represent the alternatives with anyOf or oneOf"
        )

    for expected_type, keywords in KEYWORD_TYPES.items():
        present = sorted(keys & keywords)
        if present and not type_matches(schema.get("type"), expected_type):
            problems.append(
                f"{location}: keyword(s) {', '.join(present)} require explicit type {expected_type!r}; "
                f"found {schema.get('type')!r}"
            )

    if "required" in schema:
        required = schema.get("required")
        properties = schema.get("properties")
        declared = set(properties) if isinstance(properties, dict) else set()
        if isinstance(required, list):
            for property_name in required:
                if property_name not in declared:
                    problems.append(
                        f"{location}: required property {property_name!r} is not declared in this schema's properties"
                    )

    for keyword in SCHEMA_MAP_KEYWORDS:
        mapping = schema.get(keyword)
        if isinstance(mapping, dict):
            for name, child in mapping.items():
                audit_schema(child, f"{location}/{keyword}/{name}", problems)

    for keyword in SINGLE_SCHEMA_KEYWORDS:
        child = schema.get(keyword)
        if isinstance(child, dict):
            audit_schema(child, f"{location}/{keyword}", problems)

    for keyword in SCHEMA_ARRAY_KEYWORDS:
        children = schema.get(keyword)
        if isinstance(children, list):
            for index, child in enumerate(children):
                audit_schema(child, f"{location}/{keyword}/{index}", problems)


def main() -> int:
    files = sorted(SCHEMA_ROOT.rglob("*.schema.json"))
    if not files:
        print(f"ERROR: no schemas found under {SCHEMA_ROOT}", file=sys.stderr)
        return 2

    problems: list[str] = []
    for path in files:
        relative = path.relative_to(ROOT).as_posix()
        try:
            schema = json.loads(path.read_text(encoding="utf-8"))
        except (OSError, json.JSONDecodeError) as error:
            problems.append(f"{relative}: could not read JSON: {error}")
            continue
        if not isinstance(schema, dict):
            problems.append(f"{relative}: schema root must be an object")
            continue
        if schema.get("$schema") != DRAFT:
            problems.append(f"{relative}: expected Draft 2020-12 $schema declaration")
        audit_schema(schema, relative, problems)

    if problems:
        print(f"FAIL: {len(problems)} schema keyword/type issue(s) across {len(files)} schemas.")
        for problem in problems:
            print(f"- {problem}")
        return 1

    print(f"Audited {len(files)} Draft 2020-12 schemas.")
    print("PASS: all type-specific keywords have explicit matching types.")
    print("PASS: all required property names are declared in their schema scopes.")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
