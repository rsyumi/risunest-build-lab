import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { createServerSyncController } from "./serverSyncController";
import { connectServerSync } from "./serverSyncConnectFlow";
import { createServerSyncScheduler } from "./serverSyncScheduler";
import type { ServerCycle, ServerStatus, ServerSyncFacade } from "./serverSync";

const head = {
  libraryId: "library",
  epoch: "epoch",
  seq: "0",
  headId: "head",
  minRetainedSeq: "0",
  sections: {
    hypa: { stateId: "hypa-state", changedSeq: "0", gcFloor: "0" },
    library: { stateId: "library-state", changedSeq: "0", gcFloor: "0" },
    "local-plugins": { stateId: "plugins-state", changedSeq: "0", gcFloor: "0" },
  },
};
function fixture() {
  const status: ServerStatus = {
    localRevision: 0,
    reconciling: false,
    configured: true,
    endpoint: "http://localhost",
    libraryId: "library",
    deviceId: "device",
    head,
    dirtyRecords: 0,
    pendingDeviceSections: false,
    fullScan: false,
    registrationRequired: false,
    operationPending: false,
  };
  const result: ServerCycle = {
    endpoint: "http://localhost",
    phase: "idle",
    localRevision: 0,
    head,
    conflictCount: 0,
    conflicts: [],
    appliedRecords: 0,
    proposedRecords: 0,
  };
  const cycle = vi.fn(async () => result);
  const controller = createServerSyncController({
    status: async () => status,
    bind: async () => status,
    reregister: async () => status,
    cycle,
    cancel: async () => {},
    needsRefresh: () => false,
  } as unknown as ServerSyncFacade);
  let available = true;
  const scheduler = createServerSyncScheduler(controller, {
    available: () => available,
    random: () => 0.5,
  });
  return {
    controller,
    scheduler,
    cycle,
    result,
    setAvailable: (value: boolean) => {
      available = value;
    },
  };
}
beforeEach(() => vi.useFakeTimers());
afterEach(() => vi.useRealTimers());
describe("server sync scheduler", () => {
  it.each([false, true])("holds automatic sync until connection policy is saved (replacing=%s)", async (replacing) => {
    const f = fixture();
    let release!: () => void;
    const policy = vi.fn(() => new Promise<void>((resolve) => { release = resolve; }));
    const connecting = connectServerSync(f.controller, policy, {
      config: { endpoint: "http://localhost", libraryId: "library", deviceId: "device", token: "a".repeat(64) },
      residency: "remote",
      replacing,
    });
    await vi.advanceTimersByTimeAsync(0);
    expect(policy).toHaveBeenCalledOnce();
    expect(f.cycle).not.toHaveBeenCalled();
    expect(f.controller.snapshot().connecting).toBe(true);
    expect(f.controller.canAutoSync()).toBe(false);
    expect(f.controller.canRestore()).toBe(false);
    f.scheduler.resume();
    f.scheduler.localCommit();
    f.scheduler.remoteHint();
    await vi.advanceTimersByTimeAsync(60_000);
    expect(f.cycle).not.toHaveBeenCalled();
    await expect(f.controller.synchronize()).rejects.toMatchObject({ code: "library-operation-busy" });
    release();
    await connecting;
    await vi.advanceTimersByTimeAsync(0);
    expect(f.cycle).toHaveBeenCalledTimes(1);
    expect(f.controller.snapshot()).toMatchObject({ connecting: false, error: "" });
    f.scheduler.stop();
  });
  it("keeps automatic sync paused after policy failure and allows a connection retry", async () => {
    const f = fixture();
    const request = {
      config: { endpoint: "http://localhost", libraryId: "library", deviceId: "device", token: "a".repeat(64) },
      residency: "remote" as const,
    };
    await expect(connectServerSync(f.controller, async () => {
      throw { code: "library-operation-busy" };
    }, request)).rejects.toMatchObject({ code: "library-operation-busy" });
    f.scheduler.resume();
    await vi.advanceTimersByTimeAsync(60_000);
    expect(f.cycle).not.toHaveBeenCalled();
    expect(f.controller.snapshot()).toMatchObject({ connecting: false, paused: true });
    await connectServerSync(f.controller, async () => {}, request);
    await vi.advanceTimersByTimeAsync(0);
    expect(f.cycle).toHaveBeenCalledTimes(1);
    f.scheduler.stop();
  });
  it("checks after initialization and lengthens idle polls without reading every local object", async () => {
    const f = fixture();
    await f.controller.initialize();
    await vi.advanceTimersByTimeAsync(0);
    expect(f.cycle).toHaveBeenCalledTimes(1);
    await vi.advanceTimersByTimeAsync(59_999);
    expect(f.cycle).toHaveBeenCalledTimes(1);
    await vi.advanceTimersByTimeAsync(1);
    expect(f.cycle).toHaveBeenCalledTimes(2);
    await vi.advanceTimersByTimeAsync(119_999);
    expect(f.cycle).toHaveBeenCalledTimes(2);
    await vi.advanceTimersByTimeAsync(1);
    expect(f.cycle).toHaveBeenCalledTimes(3);
    f.scheduler.stop();
  });
  it("debounces saves at 500 ms and bounds continuous edits to five seconds", async () => {
    const f = fixture();
    await f.controller.initialize();
    await vi.advanceTimersByTimeAsync(0);
    for (let i = 0; i < 12; i += 1) {
      f.scheduler.localCommit();
      await vi.advanceTimersByTimeAsync(400);
    }
    expect(f.cycle).toHaveBeenCalledTimes(1);
    f.scheduler.localCommit();
    await vi.advanceTimersByTimeAsync(200);
    expect(f.cycle).toHaveBeenCalledTimes(2);
    f.scheduler.localCommit();
    await vi.advanceTimersByTimeAsync(499);
    expect(f.cycle).toHaveBeenCalledTimes(2);
    await vi.advanceTimersByTimeAsync(1);
    expect(f.cycle).toHaveBeenCalledTimes(3);
    f.scheduler.stop();
  });
  it("keeps a local revision arriving during transport for the next single-flight run", async () => {
    const f = fixture();
    let finish!: (value: ServerCycle) => void;
    f.cycle.mockImplementationOnce(
      () =>
        new Promise((resolve) => {
          finish = resolve;
        }),
    );
    await f.controller.initialize();
    await vi.advanceTimersByTimeAsync(0);
    f.scheduler.localCommit();
    await vi.advanceTimersByTimeAsync(800);
    expect(f.cycle).toHaveBeenCalledTimes(1);
    finish(f.result);
    await f.controller.waitForIdle();
    await vi.advanceTimersByTimeAsync(0);
    expect(f.cycle).toHaveBeenCalledTimes(2);
    f.scheduler.stop();
  });
  it("backs off failures despite edits, resets on manual retry, and stops unauthorized automatic attempts", async () => {
    const f = fixture();
    f.cycle.mockRejectedValue({ code: "server-unreachable" });
    await f.controller.initialize();
    await vi.advanceTimersByTimeAsync(0);
    f.scheduler.localCommit();
    await vi.advanceTimersByTimeAsync(999);
    expect(f.cycle).toHaveBeenCalledTimes(1);
    await vi.advanceTimersByTimeAsync(1);
    expect(f.cycle).toHaveBeenCalledTimes(2);
    f.scheduler.localCommit();
    await vi.advanceTimersByTimeAsync(1999);
    expect(f.cycle).toHaveBeenCalledTimes(2);
    await vi.advanceTimersByTimeAsync(1);
    expect(f.cycle).toHaveBeenCalledTimes(3);
    await f.controller.synchronize();
    await vi.advanceTimersByTimeAsync(1000);
    expect(f.cycle).toHaveBeenCalledTimes(5);
    f.cycle.mockRejectedValue({ code: "unauthorized", retryable: false });
    await f.controller.synchronize();
    await vi.advanceTimersByTimeAsync(300_000);
    expect(f.cycle).toHaveBeenCalledTimes(6);
    f.scheduler.stop();
  });
  it("stops retrying a rejection the same attempt would receive again", async () => {
    const f = fixture();
    f.cycle.mockRejectedValue({
      code: "invalid-control-schema",
      status: 400,
      retryable: false,
    });
    await f.controller.initialize();
    await vi.advanceTimersByTimeAsync(0);
    f.scheduler.localCommit();
    await vi.advanceTimersByTimeAsync(999);
    expect(f.cycle).toHaveBeenCalledTimes(1);
    expect(f.controller.snapshot().errorRetryable).toBe(false);
    // Neither backoff nor an ongoing edit nor a remote notice may start another.
    await vi.advanceTimersByTimeAsync(300_000);
    f.scheduler.localCommit();
    f.scheduler.remoteHint();
    await vi.advanceTimersByTimeAsync(300_000);
    expect(f.cycle).toHaveBeenCalledTimes(1);
    // The manual action clears the error itself, which releases the block.
    f.cycle.mockResolvedValue(f.result);
    await f.controller.synchronize();
    expect(f.cycle).toHaveBeenCalledTimes(2);
    f.scheduler.resume();
    await vi.advanceTimersByTimeAsync(0);
    expect(f.cycle).toHaveBeenCalledTimes(3);
    f.scheduler.stop();
  });
  it("keeps backing off a failure that is worth another attempt", async () => {
    const f = fixture();
    f.cycle.mockRejectedValue({
      code: "server-unreachable",
      status: 503,
      retryable: true,
    });
    await f.controller.initialize();
    await vi.advanceTimersByTimeAsync(0);
    f.scheduler.localCommit();
    await vi.advanceTimersByTimeAsync(999);
    expect(f.cycle).toHaveBeenCalledTimes(1);
    await vi.advanceTimersByTimeAsync(1);
    expect(f.cycle).toHaveBeenCalledTimes(2);
    f.scheduler.stop();
  });
  it("reaches the same state from polling alone when no notification arrives", async () => {
    const hinted = fixture();
    const silent = fixture();
    await hinted.controller.initialize();
    await silent.controller.initialize();
    await vi.advanceTimersByTimeAsync(0);
    expect(hinted.cycle).toHaveBeenCalledTimes(1);
    expect(silent.cycle).toHaveBeenCalledTimes(1);
    // One side is told the remote moved; the other is told nothing at all.
    hinted.scheduler.remoteHint();
    await vi.advanceTimersByTimeAsync(250);
    expect(hinted.cycle).toHaveBeenCalledTimes(2);
    expect(silent.cycle).toHaveBeenCalledTimes(1);
    await vi.advanceTimersByTimeAsync(60_000);
    expect(silent.cycle).toHaveBeenCalledTimes(2);
    expect(hinted.controller.snapshot().result).toEqual(
      silent.controller.snapshot().result,
    );
    hinted.scheduler.stop();
    silent.scheduler.stop();
  });
  it("coalesces repeated notifications and keeps a failing connection backed off", async () => {
    const f = fixture();
    await f.controller.initialize();
    await vi.advanceTimersByTimeAsync(0);
    for (let i = 0; i < 10; i += 1) f.scheduler.remoteHint();
    await vi.advanceTimersByTimeAsync(250);
    expect(f.cycle).toHaveBeenCalledTimes(2);
    f.cycle.mockRejectedValue({ code: "server-unreachable" });
    f.scheduler.remoteHint();
    await vi.advanceTimersByTimeAsync(250);
    expect(f.cycle).toHaveBeenCalledTimes(3);
    f.scheduler.remoteHint();
    await vi.advanceTimersByTimeAsync(999);
    expect(f.cycle).toHaveBeenCalledTimes(3);
    await vi.advanceTimersByTimeAsync(1);
    expect(f.cycle).toHaveBeenCalledTimes(4);
    f.scheduler.stop();
  });
  it("does no background work and resumes immediately while preserving manual pause", async () => {
    const f = fixture();
    await f.controller.initialize();
    await vi.advanceTimersByTimeAsync(0);
    f.setAvailable(false);
    f.scheduler.suspend();
    f.scheduler.localCommit();
    await vi.advanceTimersByTimeAsync(300_000);
    expect(f.cycle).toHaveBeenCalledTimes(1);
    f.setAvailable(true);
    f.scheduler.resume();
    await vi.advanceTimersByTimeAsync(0);
    expect(f.cycle).toHaveBeenCalledTimes(2);
    await f.controller.pause();
    f.scheduler.resume();
    await vi.advanceTimersByTimeAsync(300_000);
    expect(f.cycle).toHaveBeenCalledTimes(2);
    f.scheduler.stop();
  });
});
