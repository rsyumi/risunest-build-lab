import { beforeEach, describe, expect, it, vi } from "vitest";

const native = vi.hoisted(() => ({
  invoke: vi.fn(),
  getIdentifier: vi.fn(),
  exists: vi.fn(),
  mkdir: vi.fn(),
  readFile: vi.fn(),
  writeFile: vi.fn(),
  revision: 0,
  owner: undefined as Uint8Array | undefined,
  calls: [] as string[],
}));

vi.mock("@tauri-apps/api/core", () => ({ invoke: native.invoke }));
vi.mock("@tauri-apps/api/app", () => ({ getIdentifier: native.getIdentifier }));
vi.mock("@tauri-apps/api/path", () => ({ join: async (...parts: string[]) => parts.join("/") }));
vi.mock("@tauri-apps/plugin-fs", () => ({
  exists: native.exists,
  mkdir: native.mkdir,
  readFile: native.readFile,
  writeFile: native.writeFile,
}));
vi.mock("@tauri-apps/plugin-os", () => ({ platform: () => "ios" }));
vi.mock("../../src/ts/storage/nativePaths", () => ({ nativeDataPath: async () => "/synthetic/ios-bench" }));

import { guard, initialize } from "./contracts";

const owner = "io.github.rsyumi.risunest.ios.bench:legacy-restore-v1";

beforeEach(() => {
  vi.clearAllMocks();
  native.revision = 0;
  native.owner = undefined;
  native.calls = [];
  native.getIdentifier.mockResolvedValue("io.github.rsyumi.risunest.ios.bench");
  native.exists.mockImplementation(async () => native.owner !== undefined);
  native.readFile.mockImplementation(async () => native.owner);
  native.mkdir.mockResolvedValue(undefined);
  native.writeFile.mockImplementation(async (_path: string, bytes: Uint8Array) => {
    native.calls.push("owner-write");
    native.owner = bytes.slice();
  });
  native.invoke.mockImplementation(async (command: string) => {
    native.calls.push(command);
    if (command === "pds_open") return { revision: native.revision };
    if (command === "pds_replace_begin") return { stagingId: "synthetic-stage" };
    if (command.startsWith("pds_replace_put_") || command === "pds_replace_add_characters") return;
    if (command === "pds_replace_commit") {
      expect(new TextDecoder().decode(native.owner)).toBe(owner);
      return { revision: ++native.revision };
    }
    if (command === "pds_commit_raw") throw new Error("Malformed synthetic request");
    throw new Error(`Unexpected synthetic command: ${command}`);
  });
});

describe("iOS synthetic profile ownership", () => {
  it("establishes fresh ownership before an earlier contract writes, then admits the legacy guard", async () => {
    await initialize();
    expect(native.revision).toBe(1);
    expect(native.calls.indexOf("owner-write")).toBeLessThan(native.calls.indexOf("pds_replace_begin"));
    await guard();
    expect(native.writeFile).toHaveBeenCalledTimes(1);
    expect(new TextDecoder().decode(native.owner)).toBe(owner);
  });

  it("refuses an unknown nonempty installed profile without claiming or mutating it", async () => {
    native.revision = 7;
    await expect(initialize()).rejects.toThrow("Install a fresh isolated benchmark profile before memory measurement");
    expect(native.mkdir).not.toHaveBeenCalled();
    expect(native.writeFile).not.toHaveBeenCalled();
    expect(native.calls).toEqual(["pds_open"]);
    expect(native.revision).toBe(7);
  });

  it("refuses a foreign bundle before accessing profile authority", async () => {
    native.getIdentifier.mockResolvedValue("synthetic.foreign");
    await expect(guard()).rejects.toThrow("isolated benchmark identifier required");
    expect(native.exists).not.toHaveBeenCalled();
    expect(native.invoke).not.toHaveBeenCalled();
    expect(native.writeFile).not.toHaveBeenCalled();
  });

  it("refuses a mismatched owner without replacing its proof or writing the store", async () => {
    native.owner = new TextEncoder().encode("synthetic-foreign-owner");
    await expect(initialize()).rejects.toThrow("Synthetic profile ownership mismatch");
    expect(native.writeFile).not.toHaveBeenCalled();
    expect(native.invoke).not.toHaveBeenCalled();
    expect(new TextDecoder().decode(native.owner)).toBe("synthetic-foreign-owner");
  });

  it("accepts the exact retained owner after earlier synthetic revisions without replacing proof", async () => {
    native.owner = new TextEncoder().encode(owner);
    native.revision = 9;
    await guard();
    expect(native.writeFile).not.toHaveBeenCalled();
    expect(native.invoke).not.toHaveBeenCalled();
    expect(native.revision).toBe(9);
  });
});
