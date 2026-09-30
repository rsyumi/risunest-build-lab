import { test } from "node:test";
import assert from "node:assert/strict";
import { mkdtempSync, writeFileSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import path from "node:path";
import { verifyUnicode, verifyDirectory } from "./verify-unicode-results.mjs";

function fixture(platform = "macos") {
  const nfd = "한글-é-Å";
  const points = (text) => Array.from(text, (character) => character.codePointAt(0));
  const cases = ["json", "raw"].map((route) => {
    const key = points(`synthetic-unicode-persistence-${route}-${nfd}`);
    const nested = points(`key-${nfd}`);
    const value = points(`${nfd}|가|ö|🐿️`);
    return {
      route, keyEqual: true, nestedKeyEqual: true, valueEqual: true, exactCodePoints: true,
      expectedKeyCodePoints: key, actualKeyCodePoints: [key],
      expectedNestedKeyCodePoints: nested, actualNestedKeyCodePoints: [nested],
      expectedValueCodePoints: value, actualValueCodePoints: value,
      transport: { command: route === "json" ? "pds_commit" : "pds_commit_raw",
        body: route === "json" ? "object" : "bytes", serializedArgumentBytes: route === "json" ? 500 : 4 * 1024 * 1024 },
    };
  });
  const immediate = { schema: "synthetic-unicode-persistence-v1", platform, revision: 3, cases, exactCodePoints: true };
  return { immediate, reloaded: { ...structuredClone(immediate), stage: "reload", paddingEqual: true } };
}

test("accepts both native commit bodies and exact decomposed code points after reload", () => {
  for (const platform of ["macos", "linux", "ios"]) {
    const { immediate, reloaded } = fixture(platform);
    verifyUnicode(immediate, reloaded, platform);
  }
});

test("refuses stale, incomplete, normalized and wrongly routed results despite success flags", () => {
  for (const mutate of [
    (value) => delete value.schema,
    (value) => value.cases.pop(),
    (value) => value.cases[0].actualValueCodePoints = [0x00e9],
    (value) => value.cases[0].expectedKeyCodePoints = [0x00e9],
    (value) => value.cases[1].transport.command = "pds_commit",
    (value) => value.cases[1].transport.body = "object",
    (value) => value.cases[1].transport.serializedArgumentBytes = 500,
    (value) => value.cases[0].transport.serializedArgumentBytes = 4 * 1024 * 1024,
    (value) => value.paddingEqual = false,
    (value) => value.revision = 2,
    (value) => value.platform = "ios",
  ]) {
    const { immediate, reloaded } = fixture();
    mutate(reloaded);
    assert.throws(() => verifyUnicode(immediate, reloaded, "macos"));
  }
});

test("requires the actual platform report files and one commit/reload stage", (t) => {
  const directory = mkdtempSync(path.join(tmpdir(), "build-lab-unicode-results-"));
  t.after(() => rmSync(directory, { recursive: true, force: true }));
  for (const platform of ["macos", "linux", "ios"]) {
    const { immediate, reloaded } = fixture(platform);
    const initial = { stage: "persistence", result: { unicode: immediate } };
    const later = { stage: "reload", result: { unicode: reloaded } };
    if (platform === "linux") {
      writeFileSync(path.join(directory, "persistence.json"), JSON.stringify(initial.result));
      writeFileSync(path.join(directory, "reload.json"), JSON.stringify(later.result));
    } else if (platform === "macos") {
      writeFileSync(path.join(directory, "contracts.jsonl"), [initial, later].map(JSON.stringify).join("\n") + "\n");
    } else {
      writeFileSync(path.join(directory, "ios-contracts.jsonl"), JSON.stringify(initial) + "\n");
      writeFileSync(path.join(directory, "ios-reload.jsonl"), JSON.stringify(later) + "\n");
    }
    assert.equal(verifyDirectory(platform, directory).reloaded, true);
  }
  assert.throws(() => verifyDirectory("windows", directory), /unsupported platform/);
  writeFileSync(path.join(directory, "contracts.jsonl"), JSON.stringify({ stage: "complete" }) + "\n");
  assert.throws(() => verifyDirectory("macos", directory), /expected one persistence/);
});
