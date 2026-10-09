import { describe, expect, it, vi } from "vitest";
import {
  createServerSyncNavigationQueue,
  parseServerSyncDeepLink,
} from "./serverSyncDeepLink";
import {
  validateServerSyncEndpoint,
  validateServerSyncId,
} from "./serverSyncConnection";

const base = "risunestlocal://sync-server/connect";
describe("server connection navigation", () => {
  it("opens an empty form and normalizes optional public endpoint and identity", () => {
    expect(parseServerSyncDeepLink(base)).toEqual({});
    expect(
      parseServerSyncDeepLink(
        `${base}?endpoint=${encodeURIComponent("https://sync.example.test/base")}&libraryId=lib_1`,
      ),
    ).toEqual({
      endpoint: "https://sync.example.test/base/",
      libraryId: "lib_1",
    });
  });
  it.each([
    "risunestlocal://peer-clone/v2",
    `${base}?token=secret`,
    `${base}?key=secret`,
    `${base}?deviceId=device`,
    `${base}?libraryId=a&libraryId=b`,
    `${base}?unknown=x`,
    `${base}#`,
    `${base}#secret`,
    `${base}?libraryId=`,
    `${base}?libraryId=a%2Fb`,
    `${base}?endpoint=${encodeURIComponent("https://user:pass@example.test")}`,
    `${base}?endpoint=${encodeURIComponent("https://@example.test")}`,
    `${base}?endpoint=${encodeURIComponent("https://example.test/?")}`,
    `${base}?endpoint=${encodeURIComponent("ftp://example.test")}`,
    "risunestlocal://@sync-server/connect",
    "risunestlocal://user@sync-server/connect",
    "risunestlocal://sync-server:12/connect",
    `${base}/`,
    "https://sync-server/connect",
  ])("rejects invalid or credential-bearing navigation %s", (uri) => {
    expect(parseServerSyncDeepLink(uri)).toBeNull();
  });
  it("queues only validated public fields until local navigation is ready", () => {
    const queue = createServerSyncNavigationQueue();
    const first = vi.fn();
    expect(queue.receive(`${base}?libraryId=first`)).toBe(true);
    expect(queue.receive(`${base}?token=secret`)).toBe(false);
    const unsubscribe = queue.subscribe(first);
    expect(first).toHaveBeenCalledExactlyOnceWith({ libraryId: "first" });
    unsubscribe();
    const next = vi.fn();
    queue.subscribe(next);
    expect(next).not.toHaveBeenCalled();
    queue.receive(base);
    expect(next).toHaveBeenCalledExactlyOnceWith({});
  });
  it("uses the existing native endpoint and identity constraints", () => {
    for (const endpoint of [
      "http://127.0.0.2:4319",
      "http://[::1]:4319",
      "http://localhost:4319",
      "http://192.168.0.10:14319",
      "http://[fd00::10]:14319",
      "http://sync.internal:14319/base",
      "http://203.0.113.10:14319",
      "https://sync.example.test/base",
    ]) {
      expect(validateServerSyncEndpoint(endpoint)).toBe(`${endpoint}/`);
      expect(parseServerSyncDeepLink(`${base}?endpoint=${encodeURIComponent(endpoint)}`)).toEqual({
        endpoint: `${endpoint}/`,
      });
    }
    expect(validateServerSyncId("x".repeat(128))).toHaveLength(128);
    expect(() => validateServerSyncId("x".repeat(129))).toThrow();
    expect(() => validateServerSyncId("한글")).toThrow();
    expect(() =>
      validateServerSyncEndpoint("https://example.test/#"),
    ).toThrow();
  });
});
