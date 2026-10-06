import assert from "node:assert/strict";
import { pathToFileURL } from "node:url";

const forbiddenMarkers = [
  "macos_bench_",
  "risunest.synthetic-legacy-restore/v1",
  "syntheticMeasurementResetId",
  "ios_bench_peak_rss",
  "legacy-restore-ready:",
  "runLegacyRestoreMeasurement",
  "synthetic-unicode-persistence-v1",
  "runUnicodePersistenceProbe",
  "verifyUnicodePersistenceProbe",
  "runLeaseReleaseCheckpointSuite",
  "lease-release-ipc",
  "synthetic-lease-release-owner",
  "__pluginReviewCountInvoke",
  "__pluginReview",
  "synthetic-plugin-review-v1",
  "RisuNest synthetic plugin review",
  "synthetic-native-boundary",
  "synthetic-plugin-page-owner",
  "io.github.rsyumi.risunest.pluginreview",
  "boundary_phase",
  "boundary_finish",
  "RISUNEST_BOUNDARY_",
  "macos-synthetic-expected",
  "__RISUNEST_LINUX_BENCHMARK__",
  "__RISUNEST_TOKENIZER_BENCHMARK__",
  "__streamingSmoke",
  "__startupMetrics",
  "startupAppearance",
  "RISUNEST_APPEARANCE_PROBE",
  "__startupRecord",
  "__startupObserveCall",
  "M0-stage:",
  "risunest-m0-owner",
  "__testResponsesAPI",
  "resetOpenedFileListenersForTest",
  "VITE_TOKENIZER_BENCHMARK",
  "VITE_STREAMING_SMOKE",
  "RisuPeerCloneBridge",
  "peer_clone_prepare",
  "peer_delta_prepare",
  "peer_bidirectional_sync",
  "device_sync_start",
  "RISUNESTLOSSLESS",
  ".risulossless",
  "native_lossless_handoff_cleanup",
  "restore-lossless-backup",
  "export-lossless-backup",
  "install_python",
  "install_pip",
  "post_py_install",
  "install_py_dependencies",
  "run_py_server",
  "check_requirements_local",
  "localhost:10026",
  "tokenizeGGUFModel",
];

export function isVerificationModule(id) {
  const normalized = id.replaceAll("\\", "/").split("?")[0];
  if (normalized.includes("/node_modules/")) {
    return /\/(?:@playwright|playwright|playwright-core|@vitest|vitest|happy-dom|jsdom|fake-indexeddb|fast-check)\//.test(
      normalized,
    );
  }
  return /(?:^|\/)(?:benchmarks|tests|test-fixtures|__tests__|__fixtures__)(?:\/|$)|\.(?:test|spec|bench|testSupport|testUtils)\.(?:[cm]?[jt]sx?|svelte(?:\.[jt]s)?)$/.test(
    normalized,
  );
}

function isRemovedModule(id) {
  const normalized = id.replaceAll("\\", "/").split("?")[0];
  return (
    /\/src\/ts\/storage\/sync\/(?:peer[A-Z]|deviceSync|bidirectionalSyncPlan|conflictBackupGate)/.test(
      normalized,
    ) ||
    /\/DeviceSyncSettings\.svelte$/.test(normalized) ||
    /\/losslessBackupFileRoute[^/]*$/.test(normalized) ||
    /\/src\/ts\/process\/models\/local\.ts$/.test(normalized)
  );
}

export function assertProductionBundle(output) {
  let javascriptFiles = 0;
  let sourceMaps = 0;
  for (const item of output) {
    if (item.type === "chunk") {
      for (const id of Object.keys(item.modules)) {
        assert.ok(
          !isVerificationModule(id) && !isRemovedModule(id),
          `Verification module in ${item.fileName}: ${id}`,
        );
      }
    }
    if (/\.[cm]?js$/.test(item.fileName)) {
      javascriptFiles++;
      const code = item.type === "chunk" ? item.code : String(item.source);
      for (const marker of forbiddenMarkers)
        assert.ok(
          !code.includes(marker),
          `Verification marker in ${item.fileName}: ${marker}`,
        );
    }
    if (item.type === "asset" && item.fileName.endsWith(".map")) {
      sourceMaps++;
      const map = JSON.parse(String(item.source));
      for (const source of map.sources ?? [])
        assert.ok(
          !isVerificationModule(source) && !isRemovedModule(source),
          `Verification source in ${item.fileName}: ${source}`,
        );
      for (const content of map.sourcesContent ?? []) {
        for (const marker of forbiddenMarkers)
          assert.ok(
            !content?.includes(marker),
            `Verification source text in ${item.fileName}: ${marker}`,
          );
      }
    }
  }
  assert.ok(javascriptFiles > 0, "No JavaScript output was checked");
  return { javascriptFiles, sourceMaps };
}

export function assertMonacoBundle(output) {
  const modules = output.flatMap(item => item.type === "chunk" ? Object.keys(item.modules) : []);
  for (const item of output) {
    assert.ok(!/(?:json|css|html|ts)\.worker[.-]/.test(item.fileName), `Unused Monaco worker: ${item.fileName}`);
    if (item.type === "asset" && item.fileName.endsWith(".map")) {
      modules.push(...(JSON.parse(String(item.source)).sources ?? []));
    }
  }
  for (const id of modules) {
    const normalized = id.replaceAll("\\", "/");
    assert.ok(!/\/vs\/language\/(?:json|css|html|typescript)\//.test(normalized), `Unused Monaco language service: ${id}`);
    assert.ok(!/\/vs\/editor\/editor\.main\.js/.test(normalized), `Full Monaco entry: ${id}`);
    const basic = normalized.match(/\/vs\/basic-languages\/([^/]+)\//)?.[1];
    assert.ok(!basic || ["markdown", "lua"].includes(basic), `Unused Monaco basic language: ${id}`);
  }
  assert.ok(modules.some(id => id.replaceAll("\\", "/").includes("/vs/editor/edcore.main.js")), "Missing Monaco editor core");
  for (const language of ["markdown", "lua"]) {
    assert.ok(modules.some(id => id.replaceAll("\\", "/").includes(`/vs/basic-languages/${language}/`)), `Missing Monaco ${language}`);
  }
  const workers = output.filter(item => /editor\.worker[.-].*\.js$/.test(item.fileName));
  assert.equal(workers.length, 1, "Expected one emitted Monaco editor worker");
  return { monacoWorkers: workers.length };
}

async function main() {
  const mode = process.argv[2] ?? "desktop";
  assert.ok(
    ["desktop", "android", "production"].includes(mode),
    "Expected desktop, android or production",
  );
  const { build } = await import("vite");
  // Removed legacy switches must not be able to turn a product build into a harness.
  process.env.VITE_TOKENIZER_BENCHMARK = "true";
  process.env.VITE_STREAMING_SMOKE = "true";
  process.env.RISUNEST_APPEARANCE_PROBE = "1";
  const platform = process.argv[3];
  if (platform) process.env.TAURI_ENV_PLATFORM = platform;
  else delete process.env.TAURI_ENV_PLATFORM;
  delete process.env.TAURI_ENV_DEBUG;
  const result = await build({
    mode,
    logLevel: "error",
    build: {
      write: false,
      copyPublicDir: false,
      reportCompressedSize: false,
      sourcemap: "hidden",
    },
  });
  const output = (Array.isArray(result) ? result : [result]).flatMap(
    (result) => result.output,
  );
  console.log(JSON.stringify({ mode, ...assertProductionBundle(output), ...assertMonacoBundle(output) }));
}

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href)
  await main();
