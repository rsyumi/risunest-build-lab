import assert from "node:assert/strict";
import { mkdirSync, mkdtempSync, readFileSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { basename, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import test from "node:test";
import {
  buildLocalSyncDistribution,
  localCargoTargetDirectory,
  localSyncTarget,
  validateLocalSyncEnvironment,
} from "../../server/manager/install/local-build.mjs";

const repository = fileURLToPath(new URL("../..", import.meta.url));

test("local Sync builds default to the main checkout Cargo cache without overriding an explicit target", () => {
  const root = join("C:/", "workspace", "RisuNest");
  const worktree = join("C:/", "workspace", "worktree");
  const common = join(root, ".git");
  assert.equal(
    localCargoTargetDirectory({ commonGitDirectory: common, root: worktree }),
    join(root, "src-tauri", "target"),
  );
  assert.equal(
    localCargoTargetDirectory({
      commonGitDirectory: common,
      configuredTarget: "custom-target",
      root: worktree,
    }),
    resolve(worktree, "custom-target"),
  );
  assert.throws(
    () => localCargoTargetDirectory({ commonGitDirectory: join(root, "unknown"), root: worktree }),
    /Cannot locate/,
  );
});

test("local Sync target mapping selects the release platform and architecture", () => {
  assert.deepEqual(localSyncTarget("x86_64-pc-windows-msvc"), {
    target: "x86_64-pc-windows-msvc",
    os: "windows",
    arch: "x86_64",
    vendorTarget: "windows-x86_64",
    gui: true,
  });
  assert.deepEqual(localSyncTarget("x86_64-unknown-linux-gnu"), {
    target: "x86_64-unknown-linux-gnu",
    os: "linux",
    arch: "x86_64",
    vendorTarget: "linux-x86_64",
    gui: false,
  });
  assert.deepEqual(localSyncTarget("aarch64-apple-darwin"), {
    target: "aarch64-apple-darwin",
    os: "darwin",
    arch: "aarch64",
    vendorTarget: "darwin-aarch64",
    gui: true,
  });
  assert.throws(() => localSyncTarget("wasm32-unknown-unknown"), /Unsupported local Sync target/);
});

test("local Sync builds require only resolved Cargo output and a native host", () => {
  const target = "x86_64-pc-windows-msvc";
  assert.throws(
    () => validateLocalSyncEnvironment({ target, hostTarget: target, env: {} }),
    /CARGO_TARGET_DIR/,
  );
  assert.equal(
    validateLocalSyncEnvironment({
      target,
      hostTarget: target,
      env: { CARGO_TARGET_DIR: "C:/target" },
    }).registryUrl,
    "https://registry.rsyumi.workers.dev/",
  );
  assert.throws(
    () => validateLocalSyncEnvironment({
      target,
      hostTarget: "aarch64-apple-darwin",
      env: {
        CARGO_TARGET_DIR: "C:/target",
        RISUNEST_DEFAULT_REGISTRY_URL: "https://registry.example.com",
      },
    }),
    /native host target/,
  );
  assert.equal(validateLocalSyncEnvironment({
    target,
    hostTarget: target,
    env: {
      CARGO_TARGET_DIR: "C:/target",
      RISUNEST_DEFAULT_REGISTRY_URL: "https://registry.example.com",
    },
  }).registryUrl, "https://registry.example.com/");
});

test("local Sync orchestration reuses build and package stages and gathers every artifact", async () => {
  const root = mkdtempSync(join(tmpdir(), "risunest-local-sync-"));
  const source = join(root, "source");
  const output = join(root, "output");
  mkdirSync(source);
  const raw = join(source, "raw.zip");
  const managed = join(source, "managed.zip");
  const installer = join(source, "RisuNest Sync_1.0.0_x64-setup.exe");
  for (const path of [raw, managed, installer]) writeFileSync(path, basename(path));
  const calls = [];
  const result = await buildLocalSyncDistribution({
    target: "x86_64-pc-windows-msvc",
    outputRoot: root,
    env: {},
    hostTarget: "x86_64-pc-windows-msvc",
    publishedAt: "2026-09-20T00:00:00Z",
  }, {
    cargoTargetDirectory() {
      calls.push(["cargo-target"]);
      return join(root, "cargo");
    },
    resetDirectory(path) {
      calls.push(["reset", path]);
      mkdirSync(path, { recursive: true });
    },
    preflight() { calls.push(["preflight"]); },
    sourceCommit() { calls.push(["commit"]); return "a".repeat(40); },
    sourceCheck(input) {
      calls.push(["source", input.tag]);
      return { version: "1.0.0", compatibility: {}, vendorInput: {} };
    },
    async fetchCloudflared(input) {
      calls.push(["vendor", input.target]);
      return { executableSha256: "b".repeat(64) };
    },
    buildNativeSuite(input) {
      calls.push(["build", input.target]);
      return { target: input.target, daemon: join(root, "daemon.exe") };
    },
    packageRaw(input) {
      calls.push(["raw", input.target]);
      return raw;
    },
    packageNativeSuite(input) {
      calls.push(["package", input.cloudflaredSha256]);
      return {
        assets: [
          { download: { variant: "raw", format: "zip" }, path: raw },
          { download: { variant: "managed", format: "zip" }, path: managed },
          { download: { variant: "managed", format: "nsis" }, path: installer },
        ],
      };
    },
  });
  assert.deepEqual(calls.map(([name]) => name), [
    "cargo-target", "preflight", "reset", "commit", "source", "vendor", "build", "raw", "package",
  ]);
  assert.deepEqual(result.artifacts.map((item) => basename(item.path)).sort(), [
    "RisuNest Sync_1.0.0_x64-setup.exe", "managed.zip", "raw.zip",
  ]);
  for (const artifact of result.artifacts) assert.ok(readFileSync(artifact.path).length > 0);
  assert.equal(result.output, output);
});

test("root package scripts build complete local Sync distributions", () => {
  const scripts = JSON.parse(readFileSync(join(repository, "package.json"), "utf8")).scripts;
  assert.equal(scripts["sync:build"], undefined);
  assert.equal(
    scripts["sync:windows:build"],
    "node server/manager/install/local-build.mjs --target x86_64-pc-windows-msvc",
  );
  assert.equal(
    scripts["sync:linux:build"],
    "node server/manager/install/local-build.mjs --target x86_64-unknown-linux-gnu",
  );
  assert.equal(
    scripts["sync:macos:build"],
    "node server/manager/install/local-build.mjs --target aarch64-apple-darwin",
  );
});
