import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import path from "node:path";
import { pathToFileURL } from "node:url";

export function verifyUnicode(immediate, reloaded, platform) {
  assert(["macos", "linux", "ios"].includes(platform), "unsupported platform");
  const nfd = "\u1112\u1161\u11ab\u1100\u1173\u11af-e\u0301-A\u030a";
  const points = (value) => Array.from(value, (character) => character.codePointAt(0));
  for (const result of [immediate, reloaded]) {
    assert.equal(result?.schema, "synthetic-unicode-persistence-v1", "missing Unicode probe");
    assert.equal(result.platform, platform);
    assert.equal(result.exactCodePoints, true);
    assert.deepEqual(result.cases.map((item) => item.route), ["json", "raw"]);
    for (const item of result.cases) {
      assert.equal(item.exactCodePoints, true);
      assert.equal(item.keyEqual, true);
      assert.equal(item.nestedKeyEqual, true);
      assert.equal(item.valueEqual, true);
      assert.deepEqual(item.expectedKeyCodePoints, points(`synthetic-unicode-persistence-${item.route}-${nfd}`));
      assert.deepEqual(item.expectedNestedKeyCodePoints, points(`key-${nfd}`));
      assert.deepEqual(item.expectedValueCodePoints, points(`${nfd}|\u1100\u1161|o\u0308|\u{1f43f}\ufe0f`));
      assert.deepEqual(item.actualKeyCodePoints, [item.expectedKeyCodePoints]);
      assert.deepEqual(item.actualNestedKeyCodePoints, [item.expectedNestedKeyCodePoints]);
      assert.deepEqual(item.actualValueCodePoints, item.expectedValueCodePoints);
      assert.equal(item.transport.command, item.route === "json" ? "pds_commit" : "pds_commit_raw");
      assert.equal(item.transport.body, item.route === "json" ? "object" : "bytes");
      assert(Number.isSafeInteger(item.transport.serializedArgumentBytes) && item.transport.serializedArgumentBytes > 0);
      assert(item.route === "json" ? item.transport.serializedArgumentBytes < 2 * 1024 * 1024 : item.transport.serializedArgumentBytes > 2 * 1024 * 1024);
    }
  }
  assert.equal(reloaded.stage, "reload");
  assert.equal(reloaded.paddingEqual, true);
  assert(Number.isSafeInteger(immediate.revision) && reloaded.revision >= immediate.revision);
  assert.deepEqual(reloaded.cases, immediate.cases, "reload changed code points or transport evidence");
}

export function verifyDirectory(platform, directory) {
  const json = (file) => JSON.parse(readFileSync(path.join(directory, file), "utf8"));
  const records = (file) => readFileSync(path.join(directory, file), "utf8").trim().split("\n").map((line) => JSON.parse(line));
  let immediate, reloaded;
  if (platform === "linux") {
    immediate = json("persistence.json").unicode;
    reloaded = json("reload.json").unicode;
  } else if (platform === "macos" || platform === "ios") {
    const initial = records(platform === "macos" ? "contracts.jsonl" : "ios-contracts.jsonl");
    const later = platform === "macos" ? initial : records("ios-reload.jsonl");
    const stage = (entries, name) => {
      const matched = entries.filter((entry) => entry.stage === name);
      assert.equal(matched.length, 1, `expected one ${name} record`);
      return matched[0].result.unicode;
    };
    immediate = stage(initial, "persistence");
    reloaded = stage(later, "reload");
  } else {
    throw new Error("unsupported platform");
  }
  verifyUnicode(immediate, reloaded, platform);
  return { platform, schema: immediate.schema, routes: immediate.cases.map((item) => item.route), reloaded: true };
}

if (process.argv[1] && import.meta.url === pathToFileURL(path.resolve(process.argv[1])).href) {
  assert.equal(process.argv.length, 4, "usage: verify-unicode-results.mjs PLATFORM ARTIFACT_DIRECTORY");
  console.log(JSON.stringify(verifyDirectory(process.argv[2], process.argv[3])));
}
