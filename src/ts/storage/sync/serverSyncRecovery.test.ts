import { afterEach, expect, it, vi } from "vitest";
import { get } from "svelte/store";
import { retryServerSyncRecovery, serverSyncRecovery, setServerSyncRecovery } from "./serverSyncRecovery";

afterEach(() => {
  setServerSyncRecovery(null);
  vi.useRealTimers();
});

it("bounds automatic recovery without hiding the manual action or clearing its retained owner", async () => {
  vi.useFakeTimers();
  const retry = vi.fn(async (): Promise<void> => { throw new Error("native busy"); });
  setServerSyncRecovery(true, retry);
  await vi.advanceTimersByTimeAsync(60_000);
  expect(retry).toHaveBeenCalledTimes(3);
  expect(get(serverSyncRecovery)).toEqual({ confirmationPending: true });
  await expect(retryServerSyncRecovery()).rejects.toThrow("native busy");
  expect(retry).toHaveBeenCalledTimes(4);
  retry.mockImplementationOnce(async () => { setServerSyncRecovery(null); });
  await retryServerSyncRecovery();
  expect(get(serverSyncRecovery)).toBeNull();
});
