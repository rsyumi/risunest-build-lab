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
  assert.throws(() => assertProductionBundle([
    { type: 'asset', fileName: 'assets/worker.js', source: 'postMessage(1)' },
    { ...map(['../../src/lib/ChatScreens/chatMountProbe.testSupport.ts']), fileName: 'assets/worker.js.map' },
  ]), /Verification source/);
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

test("release builds strip development output in app, Svelte and worker code", async () => {
  const { build, createServer } = await import("vite");
  const { mkdtemp, writeFile, rm } = await import("node:fs/promises");
  const { resolve, join, dirname, basename } = await import("node:path");
  const { runInNewContext } = await import("node:vm");
  const directory = await mkdtemp(resolve(".tmp-console-build-"));
  const savedDebug = process.env.TAURI_ENV_DEBUG;
  const methods = ["log", "info", "debug", "trace", "table", "dir", "dirxml", "group", "groupCollapsed", "groupEnd", "time", "timeLog", "timeEnd", "count", "countReset", "profile", "profileEnd", "timeStamp"];
  const source = methods.map(method => 'console.' + method + '("development-only", globalThis.formatForLog());').join("\n")
    + '\nconsole.warn("warning-retained"); console.error("error-retained"); globalThis.completed = true;';
  try {
    await writeFile(join(directory, "entry.ts"), source + '\nimport { userScriptConsole } from "../src/ts/userScriptConsole"; userScriptConsole.log("script-output-retained"); import "./Component.svelte"; globalThis.worker = new Worker(new URL("./worker.ts", import.meta.url), { type: "module" });');
    await writeFile(join(directory, "worker.ts"), source);
    await writeFile(join(directory, "Component.svelte"), '<script module>console.log("svelte-development-only"); console.warn("svelte-warning-retained");</script>');
    for (const mode of ["desktop", "android", "production", "agent"]) {
      delete process.env.TAURI_ENV_DEBUG;
      const result = await build({ mode, logLevel: "silent", build: {
        write: false, copyPublicDir: false, minify: mode === "production" ? "oxc" : false, sourcemap: false,
        rolldownOptions: { input: join(directory, "entry.ts") },
      }});
      const output = (Array.isArray(result) ? result : [result]).flatMap(result => result.output);
      const scripts = output.filter(item => /\.js$/.test(item.fileName));
      assert.equal(scripts.length, 2, mode + " app and worker");
      for (const item of scripts) {
        const code = item.type === "chunk" ? item.code : String(item.source);
        assert.ok(!code.includes("development-only"), mode + " " + item.fileName);
        const observed = [];
        const sandbox = {
          console: Object.fromEntries([...methods, "warn", "error"].map(level => [level, (...args) => observed.push([level, ...args])])),
          formatForLog() { throw new Error("Release evaluated a development log argument"); },
          Worker: class {}, URL,
        };
        runInNewContext(code.replaceAll("import.meta.url", JSON.stringify("https://synthetic.invalid/entry.js")), sandbox);
        assert.equal(sandbox.completed, true);
        assert.ok(observed.some(([level, message]) => level === "warn" && message === "warning-retained"));
        assert.ok(observed.some(([level, message]) => level === "error" && message === "error-retained"));
        assert.ok(observed.every(([level, message]) => ["warn", "error"].includes(level) || message === "script-output-retained"));
        if (item.type === "chunk") assert.ok(observed.some(([level, message]) => level === "log" && message === "script-output-retained"));
      }
    }
    const server = await createServer({ mode: "agent", logLevel: "silent", server: { middlewareMode: true, watch: null, hmr: false }, optimizeDeps: { noDiscovery: true, include: [] } });
    try {
      for (const file of ["entry.ts", "worker.ts", "Component.svelte"]) {
        const result = await server.transformRequest("/" + basename(directory) + "/" + file);
        assert.match(result.code, /development-only/, "dev " + file);
      }
    } finally {
      await server.close();
    }
    process.env.TAURI_ENV_DEBUG = "true";
    const debug = await build({ mode: "agent", logLevel: "silent", build: {
      write: false, copyPublicDir: false, minify: false, sourcemap: false,
      rolldownOptions: { input: join(directory, "entry.ts") },
    }});
    const output = (Array.isArray(debug) ? debug : [debug]).flatMap(result => result.output);
    for (const item of output.filter(item => /\.js$/.test(item.fileName))) {
      assert.match(item.type === "chunk" ? item.code : String(item.source), /development-only/);
    }
  } finally {
    if (savedDebug === undefined) delete process.env.TAURI_ENV_DEBUG;
    else process.env.TAURI_ENV_DEBUG = savedDebug;
    assert.equal(dirname(directory), resolve("."));
    await rm(directory, { recursive: true, force: true });
  }
});
