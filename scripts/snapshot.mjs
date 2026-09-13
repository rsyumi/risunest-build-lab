import { execFileSync } from "node:child_process";
import { createHash } from "node:crypto";
import {
  mkdirSync,
  writeFileSync,
  readFileSync,
  readdirSync,
  lstatSync,
  chmodSync,
} from "node:fs";
import path from "node:path";
import assert from "node:assert/strict";

export const rulesVersion = "risunest-build-inputs-v1";
export const sha256 = (bytes) =>
  createHash("sha256").update(bytes).digest("hex");
export const json = (value) => JSON.stringify(value, null, 2) + "\n";
export function git(repo, args, options = {}) {
  return execFileSync(
    "git",
    [
      "-c",
      `safe.directory=${path.resolve(repo).replaceAll("\\", "/")}`,
      "-C",
      repo,
      ...args,
    ],
    { maxBuffer: 512 * 1024 * 1024, ...options },
  );
}
export function validPath(name) {
  assert(
    name &&
      !name.includes("\\") &&
      !name.includes("\0") &&
      !path.posix.isAbsolute(name),
  );
  assert(
    name
      .split("/")
      .every((p) => p && p !== "." && p !== ".." && !p.includes(":")),
  );
}
export function exclusion(name) {
  validPath(name);
  const parts = name.split("/");
  if (parts[0] === "docs") return "private-control-or-generated";
  if (
    parts.some((p) =>
      /^(?:\.git|\.github|\.agents|\.claude|\.codex|\.vscode|node_modules|target(?:-.*)?|dist(?:-.*)?|save|\.local|\.tmp|\.worktrees)$/.test(
        p,
      ),
    )
  )
    return "private-control-or-generated";
  if (
    /(?:^|\/)(?:\.env(?:\..*)?|key\.txt|keystore\.properties)$/.test(name) &&
    ![".env.agent", ".env.android", ".env.desktop"].includes(name)
  )
    return "credential-input";
  if (
    /\.(?:pem|key|p12|pfx|jks|keystore|mobileprovision|db|sqlite|sqlite3|risudat|risusave)$/i.test(
      name,
    )
  )
    return "private-data-or-signing";
  if (
    /\.(?:md|mdx|rst)$/i.test(name) &&
    !/(?:^|\/)(?:LICENSE|COPYING|NOTICE|COPYRIGHT|SOURCE)(?:\.|$)/i.test(name)
  )
    return "explanatory-document";
  if (
    name === "src-tauri/src/mainx.txt" ||
    name.startsWith("src-tauri/icon-src/")
  )
    return "unused-source";
  const roots = [
    "src",
    "src-tauri",
    "crates",
    "server",
    "public",
    "resources",
    "benchmarks",
    "tests",
    "scripts",
    "util",
  ];
  const files = [
    "LICENSE",
    ".env.agent",
    ".env.android",
    ".env.desktop",
    ".npmrc",
    "index.html",
    "package.json",
    "pnpm-lock.yaml",
    "pnpm-workspace.yaml",
    "tsconfig.json",
    "tsconfig.node.json",
    "version.json",
    "vite.config.ts",
    "vitest.config.ts",
    "vitest.setup.ts",
  ];
  if (
    !roots.includes(parts[0]) &&
    !files.includes(name) &&
    !/^(?:LICENSE|COPYING|NOTICE|COPYRIGHT)(?:\.|$)/i.test(name)
  )
    return "outside-build-inputs";
  return null;
}
export function exportSnapshot(repo, commit, lane, destination) {
  assert(/^[0-9a-f]{40}$/.test(commit), "source commit must be a full SHA");
  assert(["macos", "ios"].includes(lane), "invalid lane");
  assert.equal(
    git(repo, ["rev-parse", `${commit}^{commit}`])
      .toString()
      .trim(),
    commit,
  );
  const tree = git(repo, ["rev-parse", `${commit}^{tree}`])
    .toString()
    .trim();
  mkdirSync(destination, { recursive: true });
  assert.equal(readdirSync(destination).length, 0, "destination must be empty");
  const entries = git(repo, ["ls-tree", "-rz", commit])
    .toString("utf8")
    .split("\0")
    .filter(Boolean)
    .map((line) => {
      const [header, name] = line.split("\t");
      const [mode, type, oid] = header.split(" ");
      validPath(name);
      assert.equal(type, "blob", `unsupported tree entry: ${name}`);
      assert(
        ["100644", "100755"].includes(mode),
        `unsupported file mode: ${name}`,
      );
      return { path: name, mode, oid, reason: exclusion(name) };
    })
    .sort((a, b) => (a.path < b.path ? -1 : a.path > b.path ? 1 : 0));
  const included = [],
    excluded = [];
  const blobs = git(repo, ["cat-file", "--batch"], {
    input: entries.map((e) => e.oid).join("\n") + "\n",
  });
  let offset = 0;
  for (const entry of entries) {
    const end = blobs.indexOf(10, offset);
    const [oid, type, size] = blobs.subarray(offset, end).toString().split(" ");
    assert.equal(oid, entry.oid);
    assert.equal(type, "blob");
    offset = end + 1;
    const bytes = blobs.subarray(offset, offset + Number(size));
    offset += Number(size) + 1;
    const record = {
      path: entry.path,
      mode: entry.mode,
      bytes: bytes.length,
      sha256: sha256(bytes),
    };
    if (entry.reason) {
      excluded.push({ ...record, reason: entry.reason });
      continue;
    }
    const file = path.join(destination, entry.path);
    mkdirSync(path.dirname(file), { recursive: true });
    writeFileSync(file, bytes);
    chmodSync(file, entry.mode === "100755" ? 0o755 : 0o644);
    included.push(record);
  }
  const manifest = { included, excluded };
  const manifestBytes = json(manifest);
  const metadata = {
    schema_version: 1,
    lane,
    source_commit: commit,
    source_tree: tree,
    file_list_sha256: sha256(manifestBytes),
    exclusion_rules_version: rulesVersion,
  };
  mkdirSync(path.join(destination, ".build-lab"));
  writeFileSync(path.join(destination, ".build-lab/files.json"), manifestBytes);
  writeFileSync(
    path.join(destination, ".build-lab/source.json"),
    json(metadata),
  );
  verifySnapshot(destination, lane);
  return metadata;
}
export function verifySnapshot(root, lane) {
  assert(["macos", "ios"].includes(lane), "invalid expected lane");
  const metadata = JSON.parse(
    readFileSync(path.join(root, ".build-lab/source.json")),
  );
  assert.equal(metadata.schema_version, 1);
  assert.equal(metadata.lane, lane, "snapshot lane mismatch");
  assert.match(metadata.source_commit, /^[0-9a-f]{40}$/);
  assert.match(metadata.source_tree, /^[0-9a-f]{40}$/);
  assert.equal(metadata.exclusion_rules_version, rulesVersion);
  const bytes = readFileSync(path.join(root, ".build-lab/files.json"));
  assert.equal(
    sha256(bytes),
    metadata.file_list_sha256,
    "manifest hash mismatch",
  );
  const manifest = JSON.parse(bytes);
  const expected = new Set([".build-lab/source.json", ".build-lab/files.json"]);
  const seen = new Set();
  for (const [kind, records] of Object.entries(manifest)) {
    assert(["included", "excluded"].includes(kind));
    let previous = "";
    for (const record of records) {
      validPath(record.path);
      assert(
        record.path > previous && !seen.has(record.path),
        "manifest order or duplicate",
      );
      previous = record.path;
      seen.add(record.path);
      assert(["100644", "100755"].includes(record.mode));
      assert(Number.isSafeInteger(record.bytes) && record.bytes >= 0);
      assert.match(record.sha256, /^[0-9a-f]{64}$/);
      if (kind === "excluded") {
        assert.equal(exclusion(record.path), record.reason);
        assert(record.reason);
        continue;
      }
      assert.equal(exclusion(record.path), null, "forbidden snapshot input");
      const file = path.join(root, record.path);
      const stat = lstatSync(file);
      assert(stat.isFile() && !stat.isSymbolicLink());
      assert.equal(stat.size, record.bytes, `size mismatch: ${record.path}`);
      assert.equal(
        sha256(readFileSync(file)),
        record.sha256,
        `file hash mismatch: ${record.path}`,
      );
      if (process.platform !== "win32")
        assert.equal(
          Boolean(stat.mode & 0o111),
          record.mode === "100755",
          `mode mismatch: ${record.path}`,
        );
      expected.add(record.path);
    }
  }
  assert(
    Array.isArray(manifest.included) &&
      manifest.included.length > 0 &&
      Array.isArray(manifest.excluded),
  );
  function walk(dir, relative = "") {
    for (const entry of readdirSync(dir, { withFileTypes: true })) {
      if (!relative && entry.name === ".git") continue;
      const name = relative + entry.name;
      assert(!entry.isSymbolicLink(), `symlink in snapshot: ${name}`);
      if (entry.isDirectory()) walk(path.join(dir, entry.name), name + "/");
      else assert(expected.delete(name), `unexpected snapshot file: ${name}`);
    }
  }
  walk(root);
  assert.equal(expected.size, 0, "missing snapshot files");
  return metadata;
}
