import assert from "node:assert/strict";
import { mkdtempSync, rmSync, readFileSync } from "node:fs";
import { tmpdir } from "node:os";
import path from "node:path";
import { git, verifySnapshot } from "./snapshot.mjs";

const [repo, snapshot, lane, ...extra] = process.argv.slice(2);
assert(
  snapshot && lane && !extra.length,
  "Usage: commit-snapshot.mjs BUILD_LAB_REPO EXPORTED_SNAPSHOT macos|ios",
);
const metadata = verifySnapshot(snapshot, lane);
assert.equal(
  git(repo, ["remote", "get-url", "origin"]).toString().trim(),
  "https://github.com/rsyumi/risunest-build-lab.git",
);
const branch = `refs/heads/snapshots/${lane}`;
let previous;
try {
  previous = git(repo, ["rev-parse", "--verify", branch], {
    stdio: ["pipe", "pipe", "pipe"],
  })
    .toString()
    .trim();
} catch {
  previous = null;
}
const temporary = mkdtempSync(path.join(tmpdir(), "build-lab-index-"));
const env = {
  ...process.env,
  GIT_INDEX_FILE: path.join(temporary, "index"),
  GIT_WORK_TREE: path.resolve(snapshot),
};
const run = (args) =>
  git(repo, ["-c", "core.autocrlf=false", ...args], { env });
try {
  run(["read-tree", "--empty"]);
  run(["add", "--all", "--force", "--", "."]);
  const manifest = JSON.parse(
    readFileSync(path.join(snapshot, ".build-lab/files.json")),
  );
  for (const entry of manifest.included.filter((f) => f.mode === "100755"))
    run(["update-index", "--chmod=+x", "--", entry.path]);
  const tree = run(["write-tree"]).toString().trim();
  const commit = run([
    "commit-tree",
    tree,
    ...(previous ? ["-p", previous] : []),
    "-m",
    `build(${lane}): snapshot ${metadata.source_commit}`,
  ])
    .toString()
    .trim();
  run(["update-ref", branch, commit, previous ?? "0".repeat(40)]);
  console.log(
    JSON.stringify({
      lane,
      snapshot_commit: commit,
      source_commit: metadata.source_commit,
      previous_snapshot: previous,
    }),
  );
} finally {
  assert(
    path
      .resolve(temporary)
      .startsWith(path.resolve(tmpdir()) + path.sep + "build-lab-index-"),
  );
  rmSync(temporary, { recursive: true, force: true });
}
