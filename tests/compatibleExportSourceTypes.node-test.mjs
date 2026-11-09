import assert from "node:assert/strict";
import fs from "node:fs";
import path from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";
import {
  contractDirectory,
  sourceContract,
} from "../scripts/compatibleExportContract.mjs";

// Compares RisuNest's own Database types with each export contract. A value the
// types allow but the target rejects in a required position aborts the whole
// compatible export, so every such value needs a replacement in the projector.
const source = sourceContract(fileURLToPath(new URL("..", import.meta.url)));
const renamed = {
  messageNameTemplate: "groupTemplate",
  namedMessageRole: "groupOtherBotRole",
};
// What the native exporter removes or renames before projecting each record.
const records = {
  DataBase: {
    removed: ["characters", "botPresets", "pluginCustomStorage", "account", "__directory"],
    renamed,
    root: true,
  },
  character: { removed: ["chats"] },
  Chat: { removed: ["message", "rerollRecovery"] },
  Message: {},
  botPreset: { renamed },
};

// Rejected values `fallback` and the Context 4 rewrite in
// compatible_projection.rs replace instead of failing.
function replaced(key, failure, root) {
  switch (key) {
    case "adaptiveThinkingEffort":
    case "adaptive_thinking_effort":
      return failure.path === "" && failure.value === "xhigh";
    case "tokenizer":
      return failure.path === "" && typeof failure.value === "number";
    case "customFlags":
    case "flags":
      return failure.path.startsWith("[");
    case "hypaModel":
      return root && failure.path === "" && failure.value === "voyageContext4";
    default:
      return false;
  }
}

const prefix = (segment, failures) =>
  failures.map((failure) => ({ ...failure, path: segment + failure.path }));

function rejections(contract, a, b, seen) {
  const key = `${a}|${b}`;
  // A recursive type is assumed to project while it is being compared.
  if (seen.has(key)) return seen.get(key) ?? [];
  seen.set(key, null);
  const result = compare(contract, a, b, seen);
  seen.set(key, result);
  return result;
}

function compare(contract, a, b, seen) {
  const local = source.nodes[a];
  const target = contract.nodes[b];
  const fail = (value) => [{ path: "", value }];
  if (target.kind === "any" || local.kind === "never" || local.kind === "any") return [];
  if (local.kind === "union")
    return local.variants.flatMap((variant) => rejections(contract, variant, b, seen));
  if (target.kind === "union") {
    const branches = target.variants.map((variant) => rejections(contract, a, variant, seen));
    if (branches.some((failures) => failures.length === 0)) return [];
    if (local.kind === "literal") return fail(local.value);
    if (local.kind === "scalar") return fail(local.type);
    return branches.reduce((best, failures) => (failures.length < best.length ? failures : best));
  }
  switch (local.kind) {
    case "literal":
      return target.kind === "literal"
        ? target.value === local.value ? [] : fail(local.value)
        : target.kind === "scalar" && typeof local.value === target.type ? [] : fail(local.value);
    case "scalar":
      return target.kind === "scalar" && target.type === local.type ? [] : fail(local.type);
    case "array":
      return target.kind === "array"
        ? prefix("[]", rejections(contract, local.items, target.items, seen))
        : fail("array");
    case "tuple":
      if (target.kind === "array")
        return local.items.flatMap((item, index) =>
          prefix(`[${index}]`, rejections(contract, item, target.items, seen)));
      if (target.kind !== "tuple") return fail("tuple");
      return target.items.flatMap((item, index) =>
        index < local.items.length
          ? prefix(`[${index}]`, rejections(contract, local.items[index], item, seen))
          : target.optional[index] ? [] : [{ path: `[${index}]`, value: "missing" }]);
    case "object":
      return target.kind === "object" ? compareObject(contract, local, target, seen) : fail("object");
    default:
      return fail(local.kind);
  }
}

function compareObject(contract, local, target, seen, record = {}) {
  const failures = [];
  for (const [name, field] of Object.entries(local.fields)) {
    if (record.removed?.includes(name)) continue;
    const key = record.renamed?.[name] ?? name;
    const declared = target.fields[key];
    const child = declared?.node ?? target.additional;
    // Undeclared fields are dropped and reported as unsupported structural fields.
    if (!child) continue;
    const rejected = rejections(contract, field.node, child, seen)
      .filter((failure) => !(declared && replaced(key, failure, record.root)));
    // An optional field the target rejects is dropped and reported as a loss.
    if (declared?.optional) continue;
    failures.push(...prefix(declared ? `.${key}` : ".*", rejected));
  }
  if (local.additional && target.additional)
    failures.push(...prefix(".*", rejections(contract, local.additional, target.additional, seen)));
  return failures;
}

for (const target of ["risuai", "pocket"]) {
  test(`${target}: every value RisuNest types allow is accepted or replaced`, () => {
    const contract = JSON.parse(
      fs.readFileSync(path.join(contractDirectory, `${target}.json`), "utf8"),
    );
    const seen = new Map();
    const failures = Object.entries(records).flatMap(([name, record]) =>
      prefix(name, compareObject(
        contract,
        source.nodes[source.types[name]],
        contract.nodes[contract.types[name]],
        seen,
        record,
      )));
    assert.deepEqual(
      failures.map((failure) => `${failure.path} = ${JSON.stringify(failure.value)}`),
      [],
    );
  });
}
