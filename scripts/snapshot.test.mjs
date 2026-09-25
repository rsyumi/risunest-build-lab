import { test } from "node:test";
import assert from "node:assert/strict";
import {
  mkdtempSync,
  mkdirSync,
  writeFileSync,
  readFileSync,
  rmSync,
} from "node:fs";
import { tmpdir } from "node:os";
import path from "node:path";
import { execFileSync } from "node:child_process";
import { fileURLToPath } from "node:url";
import { exportSnapshot, verifySnapshot, git, exclusion } from "./snapshot.mjs";

function fixture(t) {
  const root = mkdtempSync(path.join(tmpdir(), "build-lab-synthetic-"));
  t.after(() => rmSync(root, { recursive: true, force: true }));
  const repo = path.join(root, "repo");
  mkdirSync(repo);
  git(repo, ["init", "-q"]);
  git(repo, ["config", "user.name", "Synthetic Test"]);
  git(repo, ["config", "user.email", "test@example.invalid"]);
  git(repo, ["config", "core.autocrlf", "false"]);
  function put(name, value) {
    const file = path.join(repo, name);
    mkdirSync(path.dirname(file), { recursive: true });
    writeFileSync(file, value);
  }
  put("package.json", '{"synthetic":true}\n');
  put("src/main.ts", "export const synthetic = true;\n");
  put("LICENSE", "Synthetic license fixture\n");
  put("public/token/example/SOURCE.md", "Synthetic attribution\n");
  put("README.md", "Synthetic excluded explanation\n");
  put(".github/workflows/release.yml", "synthetic\n");
  put(".env", "SYNTHETIC_ONLY=true\n");
  put(".env.agent", "VITE_SYNTHETIC=true\n");
  put("docs/private.md", "Synthetic private placeholder\n");
  put("src-tauri/key.txt", "synthetic placeholder\n");
  git(repo, ["add", "--all"]);
  git(repo, ["commit", "-qm", "test: synthetic inputs"]);
  const commit = git(repo, ["rev-parse", "HEAD"]).toString().trim();
  return { root, repo, put, commit, destination: path.join(root, "snapshot") };
}
test("exports only the selected commit, reproducibly, with licenses and every lane", (t) => {
  const f = fixture(t);
  f.put("src/main.ts", "uncommitted synthetic change");
  f.put("src/untracked.ts", "synthetic");
  const first = exportSnapshot(f.repo, f.commit, "macos", f.destination);
  assert.equal(
    readFileSync(path.join(f.destination, "src/main.ts"), "utf8"),
    "export const synthetic = true;\n",
  );
  const second = exportSnapshot(
    f.repo,
    f.commit,
    "ios",
    path.join(f.root, "ios"),
  );
  const third = exportSnapshot(
    f.repo,
    f.commit,
    "linux",
    path.join(f.root, "linux"),
  );
  const fourth = exportSnapshot(
    f.repo,
    f.commit,
    "sync",
    path.join(f.root, "sync"),
  );
  assert.equal(first.file_list_sha256, second.file_list_sha256);
  assert.equal(first.file_list_sha256, third.file_list_sha256);
  assert.equal(first.file_list_sha256, fourth.file_list_sha256);
  assert.equal(first.source_tree, second.source_tree);
  assert.equal(first.source_tree, third.source_tree);
  assert.equal(first.source_tree, fourth.source_tree);
  const manifest = JSON.parse(
    readFileSync(path.join(f.destination, ".build-lab/files.json")),
  );
  assert.deepEqual(
    manifest.included.map((f) => f.path),
    [
      ".env.agent",
      "LICENSE",
      "package.json",
      "public/token/example/SOURCE.md",
      "src/main.ts",
    ],
  );
  assert.equal(manifest.excluded.length, 5);
  assert.deepEqual(
    exportSnapshot(f.repo, f.commit, "macos", path.join(f.root, "again")),
    first,
  );
});
test("rejects wrong lane and abbreviated source revision", (t) => {
  const f = fixture(t);
  exportSnapshot(f.repo, f.commit, "macos", f.destination);
  assert.throws(() => verifySnapshot(f.destination, "ios"), /lane mismatch/);
  assert.throws(
    () => exportSnapshot(f.repo, "HEAD", "macos", path.join(f.root, "other")),
    /full SHA/,
  );
});
test("rejects content tampering, extra files, manifest tampering, and nonempty destination", (t) => {
  const f = fixture(t);
  exportSnapshot(f.repo, f.commit, "macos", f.destination);
  const file = path.join(f.destination, "src/main.ts");
  const original = readFileSync(file);
  writeFileSync(file, "bad");
  assert.throws(() => verifySnapshot(f.destination, "macos"), /size mismatch/);
  writeFileSync(file, original);
  const extra = path.join(f.destination, "extra.txt");
  writeFileSync(extra, "synthetic");
  assert.throws(() => verifySnapshot(f.destination, "macos"), /unexpected/);
  rmSync(extra);
  assert.throws(
    () => exportSnapshot(f.repo, f.commit, "macos", f.destination),
    /must be empty/,
  );
  writeFileSync(path.join(f.destination, ".build-lab/files.json"), "{}");
  assert.throws(() => verifySnapshot(f.destination, "macos"), /manifest hash/);
});
test("excludes credentials and real-data-shaped inputs while retaining binary build inputs", () => {
  for (const file of [
    "src/test.db",
    "src/a.p12",
    "src/.env.local",
    "src-tauri/target/a.o",
    "public/save/a.json",
    "src/private.key",
    "AGENTS.md",
    ".codex/config.toml",
  ])
    assert(exclusion(file), file);
  for (const file of [
    "src/ts/rpack/rpack_map.bin",
    "src-tauri/fixtures/synthetic.json",
    "src/etc/docs/docs_text.cbs",
    "crates/lib/LICENSE",
    ".env.agent",
  ])
    assert.equal(exclusion(file), null, file);
});
test("rejects symbolic Git entries instead of copying outside the snapshot", (t) => {
  const f = fixture(t);
  const oid = git(f.repo, ["hash-object", "-w", "--stdin"], {
    input: "../outside",
  })
    .toString()
    .trim();
  git(f.repo, [
    "update-index",
    "--add",
    "--cacheinfo",
    `120000,${oid},src/link`,
  ]);
  git(f.repo, ["commit", "-qm", "test: symbolic input"]);
  const commit = git(f.repo, ["rev-parse", "HEAD"]).toString().trim();
  assert.throws(
    () => exportSnapshot(f.repo, commit, "macos", f.destination),
    /unsupported file mode/,
  );
});

test("snapshot commits retain lane history without changing the shared checkout or index", (t) => {
  const f = fixture(t);
  const lab = path.join(f.root, "lab");
  mkdirSync(lab);
  git(lab, ["init", "-q"]);
  git(lab, ["config", "user.name", "Synthetic Test"]);
  git(lab, ["config", "user.email", "test@example.invalid"]);
  git(lab, [
    "remote",
    "add",
    "origin",
    "https://rsyumi@github.com/rsyumi/risunest-build-lab.git",
  ]);
  writeFileSync(path.join(lab, "LICENSE"), "Synthetic control license");
  git(lab, ["add", "LICENSE"]);
  git(lab, ["commit", "-qm", "test: control"]);
  const before = git(lab, ["rev-parse", "HEAD"]).toString();
  writeFileSync(path.join(lab, "LICENSE"), "Synthetic unrelated edit");
  git(lab, ["add", "LICENSE"]);
  const index = git(lab, ["write-tree"]).toString();
  exportSnapshot(f.repo, f.commit, "macos", f.destination);
  const cli = fileURLToPath(new URL("./commit-snapshot.mjs", import.meta.url));
  const run = () =>
    JSON.parse(
      execFileSync(process.execPath, [
        cli,
        lab,
        f.destination,
        "macos",
      ]).toString(),
    );
  const first = run(),
    second = run();
  assert.equal(first.previous_snapshot, null);
  assert.equal(second.previous_snapshot, first.snapshot_commit);
  assert.equal(git(lab, ["rev-parse", "HEAD"]).toString(), before);
  assert.equal(git(lab, ["write-tree"]).toString(), index);
  assert.equal(
    git(lab, ["rev-list", "--count", second.snapshot_commit]).toString().trim(),
    "2",
  );
  assert.equal(
    git(lab, ["show", `${second.snapshot_commit}:src/main.ts`]).toString(),
    "export const synthetic = true;\n",
  );
});
