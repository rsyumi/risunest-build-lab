import { readFileSync } from "node:fs";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

const startup = vi.hoisted(() => ({ cacheBudget: 0 }));

vi.mock("./ts/storage/recoveryMode.svelte", () => ({
  decideBoot: vi.fn(async () => "normal"),
}));

vi.mock("./ts/polyfill", () => ({}));
vi.mock("core-js/actual", () => ({}));
vi.mock("./ts/storage/database.svelte", () => ({}));
vi.mock("./App.svelte", () => ({ default: {} }));
vi.mock("./ts/bootstrap", async () => {
  const { getRuntimePerformanceBudgets } =
    await import("./ts/runtimePerformanceProfile");
  startup.cacheBudget =
    getRuntimePerformanceBudgets().browserAssetDataUrlCacheBytes;
  return (await import("./ts/bootstrap.testSupport")).bootstrapModule();
});
vi.mock("./ts/hotkey", () => ({ initHotkey: vi.fn() }));
vi.mock("./preload", () => ({ preLoadCheck: vi.fn() }));
vi.mock("svelte", async (importOriginal) => ({
  ...(await importOriginal<typeof import("svelte")>()),
  mount: vi.fn(() => ({})),
}));
vi.mock("./ts/storage/deviceBackup/jobRecovery", () => ({
  resumePortableExportsAfterBootstrap: vi.fn(async () => {}),
}));
// The store module is the whole application graph, and this file mocks the library away,
// so only the carrier normalMain reads stands in for it.
vi.mock("./ts/stores.svelte", async () => {
  const { writable } = await import("svelte/store");
  return { recoveryStart: writable<(() => void) | null>(null) };
});

const deviceSettings = {
  schema: "risunest.device-settings/v1",
  performanceProfile: "low-spec",
  androidKeepAliveDuringGeneration: false,
  nativeFileLogEnabled: true,
  generationHistoryLimitEnabled: false,
  generationHistoryLimitMultiplier: 2,
  messageSendKey: "enter",
};

describe("application startup performance profile", () => {
  beforeEach(() => {
    localStorage.clear();
    document.body.innerHTML = '<div id="app"></div><div id="preloading"></div>';
    startup.cacheBudget = 0;
  });

  afterEach(() => {
    vi.unstubAllEnvs();
    vi.resetModules();
    localStorage.clear();
  });

  it("loads a persisted low-spec profile before bootstrap constructs runtime caches", async () => {
    localStorage.setItem(
      "risuNestDeviceSettings",
      JSON.stringify(deviceSettings),
    );

    // The native maintenance gate resolves before normalMain is imported.
    await (
      await import("./main")
    ).default;

    expect(startup.cacheBudget).toBe(8 * 1024 * 1024);
  }, 30_000);

  it("starts the normal app even when obsolete harness flags are set", async () => {
    vi.stubEnv("MODE", "agent");
    vi.stubEnv("VITE_TOKENIZER_BENCHMARK", "true");
    vi.stubEnv("VITE_STREAMING_SMOKE", "true");
    const { mount } = await import("svelte");
    vi.mocked(mount).mockClear();

    await (
      await import("./main")
    ).default;

    expect(mount).toHaveBeenCalledTimes(1);
    expect(document.getElementById("preloading")).toBeNull();
    expect(window).not.toHaveProperty("__streamingSmoke");
    expect(window).not.toHaveProperty("__RISUNEST_TOKENIZER_BENCHMARK__");
  }, 30_000);
});

describe("startup document", () => {
  it("does not hold first paint or the entry script on a third-party stylesheet", () => {
    // A parser-inserted stylesheet for screen blocks rendering and module scripts until it
    // answers, and a slow or blocked network can hold a remote one for many seconds.
    // Parsed as text: a DOM parser in the test environment would fetch the stylesheet.
    const links = [
      ...readFileSync("index.html", "utf8").matchAll(/<link\b[^>]*>/g),
    ].map(([tag]) =>
      Object.fromEntries(
        [...tag.matchAll(/([\w-]+)="([^"]*)"/g)].map(([, name, value]) => [
          name,
          value,
        ]),
      ),
    );
    const remote = links.filter(
      (link) =>
        link.rel?.split(/\s+/).includes("stylesheet") &&
        /^(https?:)?\/\//.test(link.href ?? ""),
    );

    for (const link of remote) {
      expect(link.media).toBe("print");
      expect(link.onload).toBe("this.media='all'");
    }
  });
});
