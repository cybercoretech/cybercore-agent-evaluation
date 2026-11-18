import { Ajv2020, type AnySchema, type ValidateFunction } from "ajv/dist/2020.js";
import addFormatsModule from "ajv-formats";
import type { FormatsPlugin } from "ajv-formats";
import { readdir, readFile } from "node:fs/promises";
import path from "node:path";
import { validateDomainFixtures } from "./domain-invariants.js";
import { fileURLToPath } from "node:url";

// Ajv and ajv-formats are CommonJS packages. Under NodeNext, their declarations
// expose a module namespace for default imports; use Ajv2020 as its named export
// and explicitly select the ajv-formats default plugin.
const addFormats = addFormatsModule.default as FormatsPlugin;

type JsonObject = Record<string, unknown>;
type SchemaEntry = { path: string; id: string; schema: JsonObject };

const here = path.dirname(fileURLToPath(import.meta.url));
const repositoryRoot = path.resolve(here, "../../../../");
const schemaRoot = path.join(repositoryRoot, "contracts/schemas/v0");
const fixtureRoot = path.join(repositoryRoot, "contracts/fixtures");

async function walkJsonFiles(directory: string): Promise<string[]> {
  const entries = await readdir(directory, { withFileTypes: true });
  const nested = await Promise.all(entries.map(async (entry) => {
    const fullPath = path.join(directory, entry.name);
    if (entry.isDirectory()) return walkJsonFiles(fullPath);
    return entry.isFile() && entry.name.endsWith(".schema.json") ? [fullPath] : [];
  }));
  return nested.flat().sort();
}

function collectRecordTypeConstants(value: unknown, found: Set<string>): void {
  if (Array.isArray(value)) {
    for (const item of value) collectRecordTypeConstants(item, found);
    return;
  }
  if (!value || typeof value !== "object") return;

  const object = value as JsonObject;
  const properties = object.properties as JsonObject | undefined;
  const recordType = properties?.record_type as JsonObject | undefined;
  if (typeof recordType?.const === "string") found.add(recordType.const);

  for (const child of Object.values(object)) collectRecordTypeConstants(child, found);
}

async function readJson(filePath: string): Promise<unknown> {
  try {
    return JSON.parse(await readFile(filePath, "utf8")) as unknown;
  } catch (error) {
    throw new Error(`Could not parse ${path.relative(repositoryRoot, filePath)}: ${String(error)}`);
  }
}

function createValidator(): Ajv2020 {
  const ajv = new Ajv2020({
    allErrors: true,
    strict: true,
    validateFormats: true,
    validateSchema: true,
    messages: true
  });
  addFormats(ajv, { mode: "full" });

  const dateTimeProbe = ajv.compile({ type: "string", format: "date-time" });
  if (dateTimeProbe("not-a-timestamp")) {
    throw new Error("date-time format checker accepted a deliberately invalid timestamp");
  }
  return ajv;
}

async function main(): Promise<void> {
  const schemaPaths = await walkJsonFiles(schemaRoot);
  if (schemaPaths.length === 0) throw new Error(`No schemas found under ${schemaRoot}`);

  const ajv = createValidator();
  const entries: SchemaEntry[] = [];
  const recordTypeToSchema = new Map<string, SchemaEntry>();
  const ids = new Set<string>();

  for (const schemaPath of schemaPaths) {
    const raw = await readJson(schemaPath);
    if (!raw || typeof raw !== "object" || Array.isArray(raw)) {
      throw new Error(`Schema is not a JSON object: ${schemaPath}`);
    }
    const schema = raw as JsonObject;
    const id = schema.$id;
    const dialect = schema.$schema;
    if (typeof id !== "string" || id.length === 0) {
      throw new Error(`Schema has no $id: ${schemaPath}`);
    }
    if (ids.has(id)) throw new Error(`Duplicate schema $id: ${id}`);
    ids.add(id);
    if (dialect !== "https://json-schema.org/draft/2020-12/schema") {
      throw new Error(`Unexpected JSON Schema dialect in ${schemaPath}: ${String(dialect)}`);
    }

    const entry = { path: schemaPath, id, schema };
    entries.push(entry);
    const recordTypes = new Set<string>();
    collectRecordTypeConstants(schema, recordTypes);
    for (const recordType of recordTypes) {
      if (recordTypeToSchema.has(recordType)) {
        throw new Error(`More than one persisted-record schema declares record_type=${recordType}`);
      }
      recordTypeToSchema.set(recordType, entry);
    }
  }

  // Register every schema before compiling any schema so local absolute and
  // relative $refs resolve without network access.
  for (const entry of entries) ajv.addSchema(entry.schema as AnySchema, entry.id);

  const compiled = new Map<string, ValidateFunction>();
  for (const entry of entries) {
    const validator = ajv.getSchema(entry.id);
    if (!validator) throw new Error(`Ajv did not compile schema ${entry.id}`);
    compiled.set(entry.id, validator);
  }

  async function validateDirectory(kind: "valid" | "invalid", expectedValid: boolean): Promise<number> {
    const directory = path.join(fixtureRoot, kind);
    const files = (await readdir(directory, { withFileTypes: true }))
      .filter((entry) => entry.isFile() && entry.name.endsWith(".json"))
      .map((entry) => path.join(directory, entry.name))
      .sort();
    if (files.length === 0) throw new Error(`No ${kind} fixtures found in ${directory}`);

    for (const fixturePath of files) {
      const record = await readJson(fixturePath);
      if (!record || typeof record !== "object" || Array.isArray(record)) {
        throw new Error(`Fixture must be a JSON object: ${fixturePath}`);
      }
      const recordType = (record as JsonObject).record_type;
      if (typeof recordType !== "string") {
        throw new Error(`Fixture has no string record_type: ${fixturePath}`);
      }
      const schemaEntry = recordTypeToSchema.get(recordType);
      if (!schemaEntry) throw new Error(`No schema declares record_type=${recordType} for ${fixturePath}`);
      const validator = compiled.get(schemaEntry.id);
      if (!validator) throw new Error(`Compiled validator missing for ${schemaEntry.id}`);

      const passed = validator(record);
      if (passed !== expectedValid) {
        const details = (validator.errors ?? []).map((error) =>
          `${error.instancePath || "/"}: ${error.message ?? "validation error"}`
        ).join("; ");
        throw new Error(
          `${path.relative(repositoryRoot, fixturePath)} unexpectedly ${passed ? "passed" : "failed"} ` +
          `${path.relative(repositoryRoot, schemaEntry.path)}${details ? `: ${details}` : ""}`
        );
      }
    }
    return files.length;
  }

  const validCount = await validateDirectory("valid", true);
  const invalidCount = await validateDirectory("invalid", false);
  console.log(`Ajv Draft 2020-12: compiled ${entries.length} schemas; ${recordTypeToSchema.size} record types mapped.`);
  console.log("Format assertions active: date-time; invalid-date-time probe rejected.");
  console.log(`PASS: ${validCount} valid fixtures accepted; ${invalidCount} invalid fixtures rejected.`);

  console.log("TypeScript domain invariant conformance:");
  await validateDomainFixtures(repositoryRoot);
}

main().catch((error: unknown) => {
  console.error(`ERROR: ${error instanceof Error ? error.message : String(error)}`);
  process.exitCode = 1;
});
