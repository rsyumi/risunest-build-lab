import assert from "node:assert/strict";
import test from "node:test";
import { assertProductionBundle, assertMonacoBundle } from "./productionBundle.mjs";

const chunk = (modules = {}, code = "export {};") => ({
  type: "chunk",
  fileName: "assets/app.js",
  modules,
  code,
});
const map = (sources, sourcesContent = []) => ({
  type: "asset",
  fileName: "assets/app.js.map",
  source: JSON.stringify({ sources, sourcesContent }),
});

test("accepts product code and RegExp.test polyfill", () => {
  assert.deepEqual(
    assertProductionBundle([
      chunk({
        "/src/main.ts": {},
        "/node_modules/core-js/modules/es.regexp.test.js": {},
      }),
    ]),
    { javascriptFiles: 1, sourceMaps: 0 },
  );
});
test("rejects a test module in a dynamic chunk and a test framework", () => {
  for (const id of [
    "/src/foo.test.ts",
    "/src/lib/ChatScreens/chatMountProbe.testSupport.ts",
    "/src/ts/process/luaWorkerPilotClient.testSupport.ts",
    "/src/lib/ComponentHarness.test.svelte",
    "/src/lib/state.test.svelte.ts",
    "/src/ts/database.testUtils.ts",
    "C:\\project\\src\\lib\\test-fixtures\\state.ts",
    "/benchmarks/streaming/fixture.ts",
    "/benchmarks/linux/main.ts",
    "/tests/support/helper.ts",
    "/node_modules/vitest/dist/index.js",
    "/node_modules/jsdom/lib/api.js",
    "/node_modules/@playwright/test/index.js",
    "/tests/native/main.ts",
    "/tests/browser/main.ts",
  ]) {
    assert.throws(
      () =>
        assertProductionBundle([
          chunk(),
          {
            ...chunk({ [id]: {} }),
            fileName: "assets/lazy.js",
            isDynamicEntry: true,
          },
        ]),
      /Verification module/,
    );
  }
});
test("rejects a worker asset containing a benchmark interface", () => {
  for (const source of [
    "window.__streamingSmoke = {}",
    "window.__RISUNEST_LINUX_BENCHMARK__ = {}",
  ]) {
    assert.throws(
      () =>
        assertProductionBundle([
          chunk(),
          {
            type: "asset",
            fileName: "assets/worker.js",
            source,
          },
        ]),
      /Verification marker/,
    );
  }
});
test("rejects test modules and removed test helpers in source maps", () => {
  assert.throws(
    () =>
      assertProductionBundle([
        chunk(),
        map(["../../benchmarks/tokenizer/main.ts"]),
      ]),
    /Verification source/,
  );
  assert.throws(
    () =>
      assertProductionBundle([
        chunk(),
        map(["../../src/main.ts"], ["export const __testResponsesAPI = {}"]),
      ]),
    /Verification source text/,
  );
});
test("rejects an empty bundle instead of claiming validation", () => {
  assert.throws(() => assertProductionBundle([]), /No JavaScript/);
});

test("rejects removed peer transport modules", () => {
  for (const id of [
    "/src/ts/storage/sync/peerClone.ts",
    "/src/ts/storage/sync/deviceSyncController.ts",
    "/src/lib/Setting/Pages/DeviceSyncSettings.svelte",
  ]) {
    assert.throws(
      () => assertProductionBundle([chunk({ [id]: {} })]),
      /module/,
    );
    assert.throws(() => assertProductionBundle([chunk(), map([id])]), /source/);
  }
  assert.throws(() =>
    assertProductionBundle([chunk({}, "window.RisuPeerCloneBridge = {}")]),
  );
});

test("rejects retired backup modules, commands, and magic in product chunks and maps", () => {
  for (const id of [
    "/src/ts/storage/losslessBackupFileRoute.ts",
    "/src/ts/storage/losslessBackupFileRouteProduction.svelte.ts",
  ]) {
    assert.throws(
      () => assertProductionBundle([chunk({ [id]: {} })]),
      /module/,
    );
    assert.throws(() => assertProductionBundle([chunk(), map([id])]), /source/);
  }
  for (const marker of [
    "RISUNESTLOSSLESS",
    ".risulossless",
    "restore-lossless-backup",
    "export-lossless-backup",
    "native_lossless_handoff_cleanup",
  ]) {
    assert.throws(() => assertProductionBundle([chunk({}, marker)]), /marker/);
    assert.throws(
      () => assertProductionBundle([chunk(), map(["/src/main.ts"], [marker])]),
      /source text/,
    );
  }
});

test("rejects retired GGUF code while preserving Pyodide scripting", () => {
  assertProductionBundle([
    chunk({ "/src/ts/process/pyworker.ts": {} }, "loadPyodide()"),
  ]);
  const local = "/src/ts/process/models/local.ts";
  assert.throws(
    () => assertProductionBundle([chunk({ [local]: {} })]),
    /module/,
  );
  assert.throws(
    () => assertProductionBundle([chunk(), map([local])]),
    /source/,
  );
  for (const marker of [
    "install_python",
    "install_pip",
    "post_py_install",
    "install_py_dependencies",
    "run_py_server",
    "check_requirements_local",
    "localhost:10026",
    "tokenizeGGUFModel",
  ]) {
    assert.throws(() => assertProductionBundle([chunk({}, marker)]), /marker/);
    assert.throws(
      () => assertProductionBundle([chunk(), map(["/src/main.ts"], [marker])]),
      /source text/,
    );
  }
});

test("worker maps reject test support even when worker code has no marker", () => {
  for (const source of ['../../src/lib/ChatScreens/chatMountProbe.testSupport.ts', '../../src/ts/process/luaWorkerPilotClient.testSupport.ts']) {
    assert.throws(() => assertProductionBundle([
      { type: 'asset', fileName: 'assets/worker.js', source: 'postMessage(1)' },
      { ...map([source]), fileName: 'assets/worker.js.map' },
    ]), /Verification source/);
  }
});

test("rejects isolated appearance and plugin probes in product workers and source maps", () => {
  for (const marker of ["startupAppearance", "RISUNEST_APPEARANCE_PROBE", "__pluginReviewCountInvoke", "__pluginReview", "synthetic-plugin-review-v1", "RisuNest synthetic plugin review", "synthetic-native-boundary", "synthetic-plugin-page-owner", "io.github.rsyumi.risunest.pluginreview"]) {
    assert.throws(() => assertProductionBundle([chunk(), {
      type: "asset", fileName: "assets/worker.js", source: marker,
    }]), /Verification marker/);
    assert.throws(() => assertProductionBundle([chunk(), map(["/src/main.ts"], [marker])]), /Verification source text/);
  }
});
test("rejects isolated persistence probes in product chunks, workers and source maps", () => {
  for (const marker of [
    "risunest.synthetic-legacy-restore/v1", "syntheticMeasurementResetId",
    "ios_bench_peak_rss", "legacy-restore-ready:", "runLegacyRestoreMeasurement",
    "synthetic-unicode-persistence-v1", "runUnicodePersistenceProbe", "verifyUnicodePersistenceProbe",
    "runLeaseReleaseCheckpointSuite", "lease-release-ipc", "synthetic-lease-release-owner",
  ]) {
    assert.throws(() => assertProductionBundle([chunk({}, marker)]), /Verification marker/);
    assert.throws(() => assertProductionBundle([chunk(), {
      type: "asset", fileName: "assets/worker.js", source: marker,
    }]), /Verification marker/);
    assert.throws(() => assertProductionBundle([chunk(), map(["/src/main.ts"], [marker])]), /Verification source text/);
  }
});

const monacoBundle = () => [
  chunk(Object.fromEntries([
    "editor/editor.api.js", "languages/definitions/markdown/register.js", "languages/definitions/lua/register.js",
  ].map(id => [`/node_modules/monaco-editor/esm/vs/${id}`, {}]))),
  { type: "asset", fileName: "assets/editor.worker-123.js", source: "self.onmessage = () => {};" },
];

test("Monaco bundle retains core, markdown, Lua and its editor worker", () => {
  assert.deepEqual(assertMonacoBundle(monacoBundle()), { monacoWorkers: 1 });
  assert.throws(() => assertMonacoBundle(monacoBundle().slice(0, 1)), /editor worker/);
  assert.throws(() => assertMonacoBundle([monacoBundle()[1]]), /editor core/);
});

test("Monaco bundle rejects unused contributions in chunks and worker maps", () => {
  for (const path of ["language/json/jsonMode.js", "language/typescript/ts.worker.js", "languages/features/json/register.js", "languages/features/typescript/register.js", "languages/features/css/register.js", "languages/features/html/register.js", "languages/definitions/python/register.js", "editor/editor.main.js", "index.js"]) {
    const id = `/node_modules/monaco-editor/esm/vs/${path}`;
    assert.throws(() => assertMonacoBundle([...monacoBundle(), chunk({ [id]: {} })]), /Monaco/);
    assert.throws(() => assertMonacoBundle([...monacoBundle(), map([id])]), /Monaco/);
  }
  assert.throws(() => assertMonacoBundle([...monacoBundle(), { type: "asset", fileName: "assets/ts.worker-123.js", source: "unused" }]), /Unused Monaco worker/);
});
