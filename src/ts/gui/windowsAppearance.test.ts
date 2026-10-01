import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { readFileSync } from "node:fs";
import {
  opaqueHex,
  resolveAppearance,
  WINDOWS_APPEARANCE_CACHE,
} from "./windowsAppearance";

const platform = vi.hoisted(() => ({
  desktop: false,
  os: "linux",
  invoke: vi.fn(),
  osType: vi.fn(),
  currentWindow: vi.fn(),
  setTheme: vi.fn(),
}));
vi.mock("../platform", () => ({
  get isTauriDesktop() { return platform.desktop; },
}));
vi.mock("@tauri-apps/api/core", () => ({ invoke: platform.invoke }));
vi.mock("@tauri-apps/plugin-os", () => ({ type: platform.osType }));
vi.mock("@tauri-apps/api/window", () => ({ getCurrentWindow: platform.currentWindow }));

const startup = readFileSync(
  "src-tauri/src/windows_appearance/startup.js",
  "utf8",
);

function rgba(value: string): [number, number, number, number] | undefined {
  if (!/^#[a-f0-9]{6}$/i.test(value)) return undefined;
  return [
    parseInt(value.slice(1, 3), 16),
    parseInt(value.slice(3, 5), 16),
    parseInt(value.slice(5, 7), 16),
    255,
  ];
}

describe("Windows native palette", () => {
  const palette = {
    bgcolor: "#301040",
    darkbg: "#200830",
    textcolor: "#eeddee",
    type: "dark" as const,
  };

  it("keeps a user palette instead of substituting a Fluent palette", () => {
    expect(resolveAppearance(palette, () => "", rgba)).toEqual({
      background: "#301040",
      caption: "#200830",
      text: "#eeddee",
      dark: true,
    });
  });

  it("uses resolved CSS overrides and falls back for non-color CSS", () => {
    const variables = {
      "--risu-theme-darkbg": " #776655 ",
      "--risu-theme-bgcolor": "url(image)",
      "--risu-theme-textcolor": "#abcdef",
    };
    expect(resolveAppearance(palette, (name) => variables[name], rgba)).toEqual(
      {
        background: "#301040",
        caption: "#776655",
        text: "#abcdef",
        dark: true,
      },
    );
  });

  it("composites transparent colors against the chosen surface", () => {
    expect(opaqueHex([255, 0, 0, 128], [0, 0, 255, 255])).toBe("#80007f");
    expect(opaqueHex([255, 255, 255, 0], [48, 16, 64, 255])).toBe("#301040");
  });

  it("uses the app's light preference independently of the OS", () => {
    expect(
      resolveAppearance({ ...palette, type: "light" }, () => "", rgba).dark,
    ).toBe(false);
  });
});

describe("Windows startup color cache", () => {
  beforeEach(() => {
    localStorage.clear();
    document.documentElement.removeAttribute("style");
    delete (window as Window & { __risunestStartupAppearanceInitialized?: boolean })
      .__risunestStartupAppearanceInitialized;
  });
  afterEach(() => {
    vi.restoreAllMocks();
    vi.unstubAllGlobals();
  });

  it("applies cached user colors before loading any app or database module", () => {
    localStorage.setItem(
      WINDOWS_APPEARANCE_CACHE,
      JSON.stringify({
        background: "#eefaff",
        caption: "#d0e8f0",
        text: "#102030",
        dark: false,
      }),
    );
    new Function(startup)();
    localStorage.setItem(WINDOWS_APPEARANCE_CACHE, "null");
    new Function(startup)();
    expect(
      document.documentElement.style.getPropertyValue("--risu-theme-darkbg"),
    ).toBe("#d0e8f0");
    expect(document.documentElement.style.colorScheme).toBe("light");
  });

  it("leaves defaults intact on first run", () => {
    new Function(startup)();
    expect(document.documentElement.getAttribute("style")).toBeNull();
  });

  it.each([
    "null",
    "{",
    '{"dark":true,"caption":"red"}',
    JSON.stringify({
      background: "#000000",
      caption: "url(secret)",
      text: "#ffffff",
      dark: true,
    }),
  ])("rejects malformed cache without echoing its content", (raw) => {
    const warning = vi.spyOn(console, "warn").mockImplementation(() => {});
    localStorage.setItem(WINDOWS_APPEARANCE_CACHE, raw);
    new Function(startup)();
    new Function(startup)();
    expect(document.documentElement.getAttribute("style")).toBeNull();
    expect(warning).toHaveBeenCalledExactlyOnceWith(
      "Could not read the Windows startup colors",
    );
  });

  it("observes the missing document root only once and disconnects after applying", () => {
    localStorage.setItem(WINDOWS_APPEARANCE_CACHE, JSON.stringify({
      background: "#eefaff", caption: "#d0e8f0", text: "#102030", dark: false,
    }));
    vi.spyOn(document, "documentElement", "get").mockReturnValueOnce(null!);
    const observer = { observe: vi.fn(), disconnect: vi.fn() };
    let applyRoot!: () => void;
    vi.stubGlobal("MutationObserver", vi.fn(function (callback: () => void) {
      applyRoot = callback;
      return observer;
    }));
    new Function(startup)();
    new Function(startup)();
    expect(observer.observe).toHaveBeenCalledExactlyOnceWith(document, { childList: true });
    applyRoot();
    expect(document.documentElement.style.colorScheme).toBe("light");
    expect(observer.disconnect).toHaveBeenCalledOnce();
  });
});

describe("rendered palette startup hint", () => {
  const palette = { bgcolor: "#ffffff", darkbg: "#f0f0f0", textcolor: "#0f172a", type: "light" as const };
  const expected = { background: "#ffffff", caption: "#776655", text: "#0f172a", dark: false };

  beforeEach(() => {
    vi.resetModules();
    vi.useFakeTimers();
    platform.desktop = false;
    platform.os = "linux";
    platform.invoke.mockReset().mockResolvedValue(undefined);
    platform.osType.mockReset().mockImplementation(() => platform.os);
    platform.setTheme.mockReset().mockResolvedValue(undefined);
    platform.currentWindow.mockReset().mockReturnValue({ setTheme: platform.setTheme });
    localStorage.clear();
    document.documentElement.removeAttribute("style");
    document.documentElement.style.setProperty("--risu-theme-darkbg", "#776655");
    vi.spyOn(CSS, "supports").mockReturnValue(true);
    let pixel = [0, 0, 0, 255];
    const context = {
      clearRect() {},
      fillRect() {},
      set fillStyle(value: string) {
        const channels = value.match(/^rgba?\((\d+),\s*(\d+),\s*(\d+)/);
        pixel = rgba(value) ?? (channels ? channels.slice(1).map(Number).concat(255) : pixel);
      },
      getImageData: () => ({ data: Uint8ClampedArray.from(pixel) }),
    };
    vi.spyOn(HTMLCanvasElement.prototype, "getContext")
      .mockReturnValue(context as unknown as CanvasRenderingContext2D);
  });
  afterEach(() => {
    vi.clearAllTimers();
    vi.useRealTimers();
    vi.restoreAllMocks();
  });

  it.each([
    ["web", false], ["linux", true], ["macos", true], ["android", false], ["ios", false],
  ] as const)("caches the rendered %s palette without a Windows invocation", async (os, desktop) => {
    platform.os = os;
    platform.desktop = desktop;
    const { scheduleWindowsAppearance } = await import("./windowsAppearance");
    scheduleWindowsAppearance(palette);
    scheduleWindowsAppearance();
    expect(localStorage.getItem(WINDOWS_APPEARANCE_CACHE)).toBeNull();
    await vi.runAllTimersAsync();
    expect(JSON.parse(localStorage.getItem(WINDOWS_APPEARANCE_CACHE)!)).toEqual(expected);
    expect(platform.invoke).not.toHaveBeenCalled();
    if (os === "macos") {
      expect(platform.setTheme).toHaveBeenCalledExactlyOnceWith("light");
    } else {
      expect(platform.currentWindow).not.toHaveBeenCalled();
    }
    if (!desktop) expect(platform.osType).not.toHaveBeenCalled();
  });

  it("caches macOS colors while native theme application is pending and deduplicates after success", async () => {
    platform.desktop = true;
    platform.os = "macos";
    let reply!: () => void;
    platform.setTheme.mockImplementationOnce(() => new Promise<void>(resolve => { reply = resolve; }));
    const { scheduleWindowsAppearance } = await import("./windowsAppearance");
    scheduleWindowsAppearance(palette);
    await vi.runAllTimersAsync();
    expect(JSON.parse(localStorage.getItem(WINDOWS_APPEARANCE_CACHE)!)).toEqual(expected);
    expect(platform.setTheme).toHaveBeenCalledExactlyOnceWith("light");
    scheduleWindowsAppearance(palette);
    await vi.runAllTimersAsync();
    expect(platform.setTheme).toHaveBeenCalledOnce();
    reply();
    await vi.runAllTimersAsync();
    expect(platform.setTheme).toHaveBeenCalledOnce();
    expect(platform.invoke).not.toHaveBeenCalled();
  });

  it("keeps the macOS startup hint after native failure and retries the same theme", async () => {
    platform.desktop = true;
    platform.os = "macos";
    platform.setTheme.mockRejectedValueOnce(new Error("unavailable"));
    const warning = vi.spyOn(console, "warn").mockImplementation(() => {});
    const { scheduleWindowsAppearance } = await import("./windowsAppearance");
    scheduleWindowsAppearance(palette);
    await vi.runAllTimersAsync();
    expect(JSON.parse(localStorage.getItem(WINDOWS_APPEARANCE_CACHE)!)).toEqual(expected);
    expect(warning).toHaveBeenCalledExactlyOnceWith("Could not apply the macOS window theme");
    scheduleWindowsAppearance(palette);
    await vi.runAllTimersAsync();
    expect(platform.setTheme.mock.calls).toEqual([["light"], ["light"]]);
    expect(platform.invoke).not.toHaveBeenCalled();
  });

  it("orders macOS theme updates while keeping the newest rendered startup hint", async () => {
    platform.desktop = true;
    platform.os = "macos";
    let reply!: () => void;
    platform.setTheme.mockImplementationOnce(() => new Promise<void>(resolve => { reply = resolve; }));
    const { scheduleWindowsAppearance } = await import("./windowsAppearance");
    scheduleWindowsAppearance({ ...palette, type: "dark" });
    await vi.runAllTimersAsync();
    scheduleWindowsAppearance(palette);
    await vi.runAllTimersAsync();
    expect(platform.setTheme).toHaveBeenCalledExactlyOnceWith("dark");
    expect(JSON.parse(localStorage.getItem(WINDOWS_APPEARANCE_CACHE)!)).toEqual(expected);
    reply();
    await vi.runAllTimersAsync();
    expect(platform.setTheme.mock.calls).toEqual([["dark"], ["light"]]);
  });

  it("keeps Windows persistence after native success and skips an unchanged palette", async () => {
    platform.desktop = true;
    platform.os = "windows";
    let reply!: () => void;
    platform.invoke.mockImplementationOnce(() => new Promise<void>(resolve => { reply = resolve; }));
    const { scheduleWindowsAppearance } = await import("./windowsAppearance");
    scheduleWindowsAppearance(palette);
    await vi.runAllTimersAsync();
    expect(platform.invoke).toHaveBeenCalledExactlyOnceWith("windows_set_appearance", { appearance: expected });
    expect(localStorage.getItem(WINDOWS_APPEARANCE_CACHE)).toBeNull();
    reply();
    await vi.runAllTimersAsync();
    expect(JSON.parse(localStorage.getItem(WINDOWS_APPEARANCE_CACHE)!)).toEqual(expected);
    scheduleWindowsAppearance(palette);
    await vi.runAllTimersAsync();
    expect(platform.invoke).toHaveBeenCalledOnce();
  });

  it("keeps the previous Windows hint when native application fails", async () => {
    platform.desktop = true;
    platform.os = "windows";
    localStorage.setItem(WINDOWS_APPEARANCE_CACHE, "previous");
    platform.invoke.mockRejectedValueOnce(new Error("unavailable"));
    const warning = vi.spyOn(console, "warn").mockImplementation(() => {});
    const { scheduleWindowsAppearance } = await import("./windowsAppearance");
    scheduleWindowsAppearance(palette);
    await vi.runAllTimersAsync();
    expect(localStorage.getItem(WINDOWS_APPEARANCE_CACHE)).toBe("previous");
    expect(warning).toHaveBeenCalledExactlyOnceWith("Could not apply the Windows window colors");
  });

  it("keeps an applied web palette when disposable hint storage fails", async () => {
    const warning = vi.spyOn(console, "warn").mockImplementation(() => {});
    vi.spyOn(localStorage, "setItem").mockImplementationOnce(() => { throw new Error("unavailable"); });
    const { scheduleWindowsAppearance } = await import("./windowsAppearance");
    scheduleWindowsAppearance(palette);
    await vi.runAllTimersAsync();
    expect(document.documentElement.style.colorScheme).toBe("light");
    expect(platform.invoke).not.toHaveBeenCalled();
    expect(warning).toHaveBeenCalledExactlyOnceWith("Could not cache the Windows startup colors");
  });
});
