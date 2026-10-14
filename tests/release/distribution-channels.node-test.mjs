import assert from "node:assert/strict";
import { readFileSync, readdirSync } from "node:fs";
import test from "node:test";

const root = new URL("../../", import.meta.url);
const workflows = new URL(".github/workflows/", root);

test("pull requests run only the standard, CodeQL and scoped native checks", () => {
  const pullRequestWorkflows = readdirSync(workflows, { withFileTypes: true })
    .filter((entry) => entry.isFile() && /\.ya?ml$/.test(entry.name))
    .filter((entry) => /^  pull_request:/m.test(readFileSync(new URL(entry.name, workflows), "utf8")))
    .map((entry) => entry.name)
    .sort();
  assert.deepEqual(pullRequestWorkflows, ["codeql.yml", "native-check.yml", "pr-check.yml"]);

  const pullRequestCheck = readFileSync(new URL("pr-check.yml", workflows), "utf8");
  assert.doesNotMatch(pullRequestCheck, /pull-requests:\s*write/);

  const nativeCheck = readFileSync(new URL("native-check.yml", workflows), "utf8").replace(/\r\n/g, "\n");
  const nativePermissions = nativeCheck.match(/^permissions:\n((?: {2,}[^\n]*\n)+)/m)?.[1];
  assert.equal(nativePermissions, "  contents: read\n");
});

test("CodeQL analyzes the native Rust sources without a product build", () => {
  const codeql = readFileSync(new URL("codeql.yml", workflows), "utf8").replace(/\r\n/g, "\n");
  assert.match(codeql, /- language: rust\n\s+build-mode: none/);
});
