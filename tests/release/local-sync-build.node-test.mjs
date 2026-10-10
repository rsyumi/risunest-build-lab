import assert from "node:assert/strict";
import { mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { basename, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import test from "node:test";
import {
  buildLocalSyncDistribution,
  formatLocalSyncDistribution,
  localCargoTargetDirectory,
  localSyncTarget,
  parseLocalSyncArguments,
  validateLocalSyncEnvironment,
} from "../../server/manager/install/local-build.mjs";

const repository = fileURLToPath(new URL("../..", import.meta.url));

test("local Sync builds default to the main checkout Cargo cache without overriding an explicit target", () => {
  const workspace = resolve(tmpdir(), "risunest-local-sync-paths");
  const root = join(workspace, "RisuNest");
  const worktree = join(workspace, "worktree");
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

test("local Sync builds require resolved Cargo output, update public key and a native host", () => {
  const target = "x86_64-pc-windows-msvc";
  assert.throws(
    () => validateLocalSyncEnvironment({ target, hostTarget: target, env: {} }),
    /CARGO_TARGET_DIR/,
  );
  assert.equal(
    validateLocalSyncEnvironment({
      target,
      hostTarget: target,
      env: { CARGO_TARGET_DIR: "C:/target", RISUNEST_UPDATE_PUBLIC_KEY: "synthetic-public-key" },
    }).registryUrl,
    "https://registry.rsyumi.workers.dev/",
  );
  assert.throws(
    () => validateLocalSyncEnvironment({
      target,
      hostTarget: "aarch64-apple-darwin",
      env: {
        CARGO_TARGET_DIR: "C:/target",
      RISUNEST_UPDATE_PUBLIC_KEY: "synthetic-public-key",
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
      RISUNEST_UPDATE_PUBLIC_KEY: "synthetic-public-key",
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
    env: { RISUNEST_UPDATE_PUBLIC_KEY: "synthetic-local-key" },
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
      assert.equal(process.env.RISUNEST_UPDATE_PUBLIC_KEY, "synthetic-local-key");
      calls.push(["build", input.target]);
      return { target: input.target, daemon: join(root, "daemon.exe") };
    },
    packageRaw(input) {
      calls.push(["raw", input.target]);
      return raw;
    },
    packageNativeSuite(input) {
      assert.equal(process.env.RISUNEST_UPDATE_PUBLIC_KEY, "synthetic-local-key");
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
  assert.equal(result.updatesEnabled, true);
  assert.equal(JSON.parse(readFileSync(join(output, "local-artifacts.json"), "utf8")).updatesEnabled, true);
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

test("missing local update key fails before preflight, output reset or downloads", async () => {
  for (const key of [undefined, '', '   ']) {
    const calls = [];
    await assert.rejects(buildLocalSyncDistribution({ target: 'x86_64-pc-windows-msvc',
      hostTarget: 'x86_64-pc-windows-msvc', env: { RISUNEST_UPDATE_PUBLIC_KEY: key } }, {
      cargoTargetDirectory: () => 'C:/target', preflight: () => calls.push('preflight'),
      resetDirectory: () => calls.push('reset'), fetchCloudflared: () => calls.push('download'),
    }), /RISUNEST_UPDATE_PUBLIC_KEY/);
    assert.deepEqual(calls, []);
  }
});

test("local Sync CLI requires an explicit development opt-out and reports its update behavior", () => {
  const target = "x86_64-pc-windows-msvc";
  assert.deepEqual(parseLocalSyncArguments(["--target", target]), { target, withoutUpdates: false });
  assert.deepEqual(parseLocalSyncArguments(["--target", target, "--without-updates"]), { target, withoutUpdates: true });
  assert.throws(() => parseLocalSyncArguments(["--without-updates"]), /--target is required/);
  assert.throws(() => parseLocalSyncArguments(["--target", target, "--without-update"]), /Unknown option/);
  const result = { output: "synthetic-output", artifacts: [{ path: "synthetic-package.zip" }] };
  assert.equal(formatLocalSyncDistribution({ ...result, updatesEnabled: false }),
    "Local Sync distribution created in synthetic-output\nUpdates are disabled for this development package. Update checks report update-not-configured.\nsynthetic-package.zip\n");
  assert.equal(formatLocalSyncDistribution({ ...result, updatesEnabled: true }),
    "Local Sync distribution created in synthetic-output\nsynthetic-package.zip\n");
});

for (const target of ["x86_64-pc-windows-msvc", "x86_64-unknown-linux-gnu", "aarch64-apple-darwin"]) {
  test(`explicit keyless local Sync validation remains scoped to ${target}`, () => {
    for (const key of [undefined, "", "synthetic-ignored-key"]) {
      const config = validateLocalSyncEnvironment({ target, hostTarget: target, withoutUpdates: true,
        env: { CARGO_TARGET_DIR: "C:/target", RISUNEST_UPDATE_PUBLIC_KEY: key } });
      assert.equal(config.publicKey, "");
      assert.equal(config.updatesEnabled, false);
    }
    assert.equal(validateLocalSyncEnvironment({ target, hostTarget: target,
      env: { CARGO_TARGET_DIR: "C:/target", RISUNEST_UPDATE_PUBLIC_KEY: "synthetic-update-key" } }).updatesEnabled, true);
  });

  test(`keyless ${target} orchestration suppresses inherited keys for build and packaging`, async (t) => {
    const root = mkdtempSync(join(tmpdir(), "risunest-local-keyless-"));
    t.after(() => rmSync(root, { recursive: true, force: true }));
    const archive = join(root, "synthetic-package.zip");
    writeFileSync(archive, "synthetic-package");
    const calls = [];
    const previous = process.env.RISUNEST_UPDATE_PUBLIC_KEY;
    try {
      process.env.RISUNEST_UPDATE_PUBLIC_KEY = "synthetic-parent-key";
      const result = await buildLocalSyncDistribution({ target, hostTarget: target, withoutUpdates: true,
        outputRoot: root, env: { RISUNEST_UPDATE_PUBLIC_KEY: "synthetic-input-key" } }, {
        cargoTargetDirectory: () => join(root, "cargo"), preflight: config => assert.equal(config.publicKey, ""),
        resetDirectory() {}, sourceCommit: () => "a".repeat(40),
        sourceCheck: () => ({ version: "1.0.0", compatibility: {}, vendorInput: {} }),
        fetchCloudflared: async () => ({ executableSha256: "b".repeat(64) }),
        buildNativeSuite(input) {
          assert.equal(process.env.RISUNEST_UPDATE_PUBLIC_KEY, "");
          calls.push("build");
          return { target: input.target, daemon: "synthetic-daemon" };
        },
        packageRaw() { assert.equal(process.env.RISUNEST_UPDATE_PUBLIC_KEY, ""); calls.push("raw"); return archive; },
        packageNativeSuite() {
          assert.equal(process.env.RISUNEST_UPDATE_PUBLIC_KEY, "");
          calls.push("package");
          return { assets: [{ download: { variant: "raw", format: "zip" }, path: archive }] };
        },
      });
      assert.deepEqual(calls, ["build", "raw", "package"]);
      assert.equal(result.updatesEnabled, false);
      assert.equal(JSON.parse(readFileSync(join(root, "output/local-artifacts.json"), "utf8")).updatesEnabled, false);
      assert.equal(process.env.RISUNEST_UPDATE_PUBLIC_KEY, "synthetic-parent-key");
    } finally {
      if (previous === undefined) delete process.env.RISUNEST_UPDATE_PUBLIC_KEY;
      else process.env.RISUNEST_UPDATE_PUBLIC_KEY = previous;
    }
  });
}

test("keyless native failure restores the outer synthetic update key", async () => {
  const previous = process.env.RISUNEST_UPDATE_PUBLIC_KEY;
  const failure = new Error("synthetic-keyless-native-failure");
  try {
    process.env.RISUNEST_UPDATE_PUBLIC_KEY = "synthetic-parent-key";
    await assert.rejects(buildLocalSyncDistribution({ target: "x86_64-pc-windows-msvc", hostTarget: "x86_64-pc-windows-msvc",
      withoutUpdates: true, env: { RISUNEST_UPDATE_PUBLIC_KEY: "synthetic-input-key" } }, {
      cargoTargetDirectory: () => "C:/target", preflight() {}, resetDirectory() {}, sourceCommit: () => "a".repeat(40),
      sourceCheck: () => ({ version: "1.0.0" }), fetchCloudflared: async () => ({}),
      buildNativeSuite() { assert.equal(process.env.RISUNEST_UPDATE_PUBLIC_KEY, ""); throw failure; },
    }), error => error === failure);
    assert.equal(process.env.RISUNEST_UPDATE_PUBLIC_KEY, "synthetic-parent-key");
  } finally {
    if (previous === undefined) delete process.env.RISUNEST_UPDATE_PUBLIC_KEY;
    else process.env.RISUNEST_UPDATE_PUBLIC_KEY = previous;
  }
});

test("local build restores the previous update key after native failure", async () => {
  const previous = process.env.RISUNEST_UPDATE_PUBLIC_KEY;
  const failure = new Error('synthetic-native-failure');
  try {
    process.env.RISUNEST_UPDATE_PUBLIC_KEY = 'outer-key';
    await assert.rejects(buildLocalSyncDistribution({ target: 'x86_64-pc-windows-msvc', hostTarget: 'x86_64-pc-windows-msvc',
      env: { RISUNEST_UPDATE_PUBLIC_KEY: 'inner-key' } }, {
      cargoTargetDirectory: () => 'C:/target', preflight() {}, resetDirectory() {}, sourceCommit: () => 'a'.repeat(40),
      sourceCheck: () => ({ version: '1.0.0' }), fetchCloudflared: async () => ({}),
      buildNativeSuite() { assert.equal(process.env.RISUNEST_UPDATE_PUBLIC_KEY, 'inner-key'); throw failure; },
    }), error => error === failure);
    assert.equal(process.env.RISUNEST_UPDATE_PUBLIC_KEY, 'outer-key');
  } finally {
    if (previous === undefined) delete process.env.RISUNEST_UPDATE_PUBLIC_KEY;
    else process.env.RISUNEST_UPDATE_PUBLIC_KEY = previous;
  }
});
