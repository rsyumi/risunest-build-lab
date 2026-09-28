import { describe, expect, it, vi } from "vitest";
import { createServerSyncFacade, type ServerCycle } from "./serverSync";
import type { CommittedApplyOutcome } from '../persistentDataRuntime';
import { retainableReplacementFence } from '../retainableReplacementFence';

const head = {
  libraryId: "library",
  epoch: "epoch",
  seq: "1",
  headId: "a".repeat(64),
  minRetainedSeq: "0",
  sections: {
    hypa: { stateId: "hypa-state", changedSeq: "0", gcFloor: "0" },
    library: { stateId: "library-state", changedSeq: "0", gcFloor: "0" },
    "local-plugins": { stateId: "plugins-state", changedSeq: "0", gcFloor: "0" },
  },
};
const result: ServerCycle = {
  endpoint: "http://localhost",
  phase: "idle",
  localRevision: 8,
  head,
  conflictCount: 0,
  conflicts: [],
  appliedRecords: 1,
  proposedRecords: 0,
};
function fixture(
  onVerifiedBytes?: (bytes: string) => void,
  onRetryableFailure?: (code: string | undefined) => void,
) {
  const trace: string[] = [];
  let fenced = false;
  const fence = {
    revision: 7,
    refreshCommittedWorkingSet: vi.fn(async (): Promise<CommittedApplyOutcome> => {
      expect(fenced).toBe(true);
      trace.push("refresh");
      return { kind: 'committed', revision: 8, projection: 'applied' };
    }),
    release: vi.fn(() => {
      fenced = false;
      trace.push("release");
    }),
  };
  const runtime = {
    flushPendingData: vi.fn(async () => {
      expect(fenced).toBe(false);
    }),
    capturePersistentMutationToken: vi.fn(async () => ({ revision: 7, mutationGeneration: 1 })),
    acquireDestructiveReplacementFence: vi.fn(async () => {
      fenced = true;
      trace.push("fence");
      return fence;
    }),
    refreshActiveWorkingSetFromStore: vi.fn(async (revision: number): Promise<CommittedApplyOutcome> => {
      expect(fenced).toBe(false);
      trace.push('read-only-refresh');
      return { kind: 'committed', revision, projection: 'applied' };
    }),
  };
  const native = vi.fn(async (command: string) => {
    trace.push(command);
    if (command === "server_sync_prepare") {
      expect(fenced).toBe(false);
      return {
        kind: "ready",
        preparationId: "prepared",
        localRevision: 7,
        head,
        appliedRecords: 1,
      };
    }
    if (command === "server_sync_activate") {
      expect(fenced).toBe(true);
      return { revision: 8, pluginsChanged: true, devicePluginsChanged: false };
    }
    if (command === "server_sync_publish") {
      expect(fenced).toBe(false);
      return result;
    }
    return undefined;
  });
  const facade = createServerSyncFacade({
    runtime,
    invoke: native as never,
    onProgress: (phase) => progress.push(phase),
    onVerifiedBytes,
    onRetryableFailure,
  });
  const progress: string[] = [];
  return { facade, native, runtime, fence, trace, progress };
}
describe("server sync activation boundary", () => {
  it("forwards native backup metadata counts while preparation is still running", async () => {
    vi.useFakeTimers();
    const { runtime } = fixture();
    const items = { done: 0, total: 230, activity: "downloadingBackupMetadata", processed: 1200, expected: 0 };
    const onCycleItems = vi.fn();
    let finish!: () => void;
    const pending = new Promise<void>((resolve) => { finish = resolve; });
    const native = vi.fn(async (command: string) => {
      if (command === "server_sync_progress_counts") return items;
      if (command === "server_sync_prepare") {
        await pending;
        return { kind: "report", result };
      }
      throw new Error(`Unexpected command: ${command}`);
    });
    const facade = createServerSyncFacade({ runtime, invoke: native as never, onCycleItems });
    const cycle = facade.cycle();
    try {
      await vi.advanceTimersByTimeAsync(1000);
      expect(onCycleItems).toHaveBeenCalledWith(items);
    } finally {
      finish();
      await cycle;
      vi.useRealTimers();
    }
  });

  it('refreshes chat changes without restarting unrelated plugins', async () => {
    const { runtime, native, fence } = fixture();
    const original = native.getMockImplementation()!;
    native.mockImplementation(async (command) => command === 'server_sync_activate'
      ? { revision: 8, pluginsChanged: false, devicePluginsChanged: false } as never
      : original(command));
    const restorePlugins = vi.fn();
    const invalidateDevicePlugins = vi.fn();
    const facade = createServerSyncFacade({ runtime, invoke: native as never, restorePlugins, invalidateDevicePlugins });
    await facade.cycle();
    expect(fence.refreshCommittedWorkingSet).toHaveBeenCalledOnce();
    expect(restorePlugins).not.toHaveBeenCalled();
    expect(invalidateDevicePlugins).not.toHaveBeenCalled();
  });

  it('invalidates a section-only application before release and retries only plugin reload', async () => {
    const { runtime, native, fence } = fixture();
    const original = native.getMockImplementation()!;
    native.mockImplementation(async (command) => {
      const reply = await original(command);
      if (command === 'server_sync_prepare') return { ...reply, appliedRecords: 0 } as never;
      if (command === 'server_sync_activate') return {
        revision: 7, pluginsChanged: true, devicePluginsChanged: true,
      } as never;
      return reply;
    });
    const invalidateDevicePlugins = vi.fn(() => expect(fence.release).not.toHaveBeenCalled());
    const restorePlugins = vi.fn().mockRejectedValueOnce(new Error('synthetic reload failure')).mockResolvedValue(undefined);
    const facade = createServerSyncFacade({ runtime, invoke: native as never, restorePlugins, invalidateDevicePlugins });
    await expect(facade.cycle()).rejects.toMatchObject({ code: 'committed-refresh-pending' });
    expect(invalidateDevicePlugins).toHaveBeenCalledOnce();
    expect(fence.release).toHaveBeenCalledOnce();
    expect(fence.refreshCommittedWorkingSet).not.toHaveBeenCalled();
    await expect(facade.cycle()).resolves.toEqual(result);
    expect(invalidateDevicePlugins).toHaveBeenCalledOnce();
    expect(restorePlugins).toHaveBeenCalledTimes(2);
    expect(runtime.refreshActiveWorkingSetFromStore).not.toHaveBeenCalled();
    expect(native.mock.calls.filter(([command]) => command === 'server_sync_activate')).toHaveLength(1);
  });
  it.each(['flush', 'token', 'fence'] as const)(
    'releases an unactivated preparation when %s fails and permits another cycle',
    async (failure) => {
      const { facade, native, runtime, fence } = fixture();
      const original = native.getMockImplementation()!;
      let prepared = false;
      native.mockImplementation(async (command) => {
        if (command === 'server_sync_prepare') {
          if (prepared) throw { code: 'preparation-pending' };
          prepared = true;
        }
        if (command === 'server_sync_cancel' || command === 'server_sync_publish') {
          prepared = false;
        }
        return original(command);
      });
      const error = new Error('synthetic local failure');
      if (failure === 'flush') {
        runtime.flushPendingData.mockResolvedValueOnce(undefined).mockRejectedValueOnce(error);
      } else if (failure === 'token') {
        runtime.capturePersistentMutationToken.mockRejectedValueOnce(error);
      } else {
        runtime.acquireDestructiveReplacementFence.mockRejectedValueOnce(error);
      }
      await expect(facade.cycle()).rejects.toMatchObject({ code: 'server-sync-failed' });
      expect(native).toHaveBeenCalledWith('server_sync_cancel');
      expect(prepared).toBe(false);
      expect(facade.needsRefresh()).toBe(false);
      expect(fence.release).not.toHaveBeenCalled();
      await expect(facade.cycle()).resolves.toEqual(result);
    },
  );
  it("does not let a stalled progress reply hold completion or update a later cycle", async () => {
    vi.useFakeTimers();
    try {
      const receive = vi.fn();
      const { facade, native } = fixture(receive);
      let finish!: (value: unknown) => void;
      let progress!: (value: unknown) => void;
      native.mockImplementation((command) => {
        if (command === "server_sync_prepare")
          return new Promise((resolve) => {
            finish = resolve;
          }) as never;
        if (command === "server_sync_verified_bytes")
          return new Promise((resolve) => {
            progress = resolve;
          }) as never;
        throw new Error("Unexpected command");
      });
      const active = facade.cycle();
      await vi.advanceTimersByTimeAsync(3000);
      expect(
        native.mock.calls.filter(
          ([command]) => command === "server_sync_verified_bytes",
        ),
      ).toHaveLength(1);
      finish({ kind: "report", result });
      await expect(active).resolves.toEqual(result);
      progress("1234");
      await Promise.resolve();
      expect(receive).not.toHaveBeenCalled();
      expect(vi.getTimerCount()).toBe(0);
    } finally {
      vi.useRealTimers();
    }
  });
  it("reports a sanitized retryable server failure while the transfer keeps running", async () => {
    vi.useFakeTimers();
    try {
      const receive = vi.fn();
      const { facade, native } = fixture(undefined, receive);
      let finish!: (value: unknown) => void;
      native.mockImplementation((command) => {
        if (command === "server_sync_prepare")
          return new Promise((resolve) => {
            finish = resolve;
          }) as never;
        if (command === "server_sync_retryable_failure")
          return Promise.resolve("storage-io") as never;
        throw new Error("Unexpected command");
      });
      const active = facade.cycle();
      await vi.advanceTimersByTimeAsync(1000);
      expect(receive).toHaveBeenCalledWith("storage-io");
      finish({ kind: "report", result });
      await expect(active).resolves.toEqual(result);
    } finally {
      vi.useRealTimers();
    }
  });
  it("reports preparation, activation and publication in their actual order", async () => {
    const { facade, progress, native } = fixture();
    await facade.cycle();
    expect(progress).toEqual([
      "saving",
      "preparing",
      "applying",
      "refreshing",
      "publishing",
    ]);
    progress.length = 0;
    native.mockResolvedValueOnce({
      kind: "report",
      result: { ...result, phase: "conflict" },
    } as never);
    await facade.cycle();
    expect(progress).toEqual(["saving", "preparing"]);
  });
  it("flushes local edits and guards recovery with the freshly read revision", async () => {
    const { facade, native, runtime } = fixture();
    native.mockResolvedValue({ localRevision: 19 } as never);
    const config = {
      endpoint: "https://example.com",
      libraryId: "library",
      deviceId: "new-device",
      token: "synthetic",
    };
    await facade.reregister(config);
    expect(runtime.flushPendingData).toHaveBeenCalledWith(
      "server-sync-recovery",
    );
    expect(native).toHaveBeenLastCalledWith("server_sync_reregister", {
      config,
      expectedRevision: 19,
    });
    await facade.reconcile();
    expect(native).toHaveBeenLastCalledWith("server_sync_reconcile", {
      expectedRevision: 19,
    });
    expect(runtime.acquireDestructiveReplacementFence).not.toHaveBeenCalled();
  });
  it("allows edits during network work and fences only activation and refresh", async () => {
    const { facade, trace, fence } = fixture();
    await expect(facade.cycle()).resolves.toEqual(result);
    expect(trace).toEqual([
      "server_sync_prepare",
      "fence",
      "server_sync_activate",
      "refresh",
      "release",
      "server_sync_publish",
    ]);
    expect(fence.refreshCommittedWorkingSet).toHaveBeenCalledWith(8);
  });
  it.each(['returned', 'thrown'] as const)("releases the physical fence after a %s refresh failure and retries read-only", async (failure) => {
    const { facade, native, fence, runtime } = fixture();
    if (failure === 'returned') {
      fence.refreshCommittedWorkingSet.mockResolvedValueOnce({
        kind: 'committed', revision: 8, projection: 'refresh-required',
      });
    } else {
      fence.refreshCommittedWorkingSet.mockRejectedValueOnce(new Error('synthetic refresh failure'));
    }
    await expect(facade.cycle()).rejects.toMatchObject({
      code: "committed-refresh-pending",
    });
    expect(fence.release).toHaveBeenCalledOnce();
    expect(facade.needsRefresh()).toBe(true);
    await expect(facade.reconcile()).rejects.toMatchObject({
      code: "committed-refresh-pending",
    });
    await expect(facade.cancel()).rejects.toMatchObject({
      code: "committed-refresh-pending",
    });
    await expect(facade.cycle()).resolves.toEqual(result);
    expect(
      native.mock.calls.filter(
        ([command]) => command === "server_sync_activate",
      ),
    ).toHaveLength(1);
    expect(fence.release).toHaveBeenCalledTimes(1);
    expect(runtime.refreshActiveWorkingSetFromStore).toHaveBeenCalledExactlyOnceWith(8);
    expect(fence.refreshCommittedWorkingSet).toHaveBeenCalledOnce();
  });
  it("releases the fence and discards preparation if local edits invalidate the prepared revision", async () => {
    const { facade, native, fence } = fixture();
    const original = native.getMockImplementation()!;
    native.mockImplementation(async (command) => {
      if (command === "server_sync_activate")
        throw { code: "local-revision-changed", status: 409 };
      return original(command);
    });
    await expect(facade.cycle()).rejects.toMatchObject({
      code: "local-revision-changed",
    });
    expect(fence.release).toHaveBeenCalledTimes(1);
    expect(native).toHaveBeenCalledWith("server_sync_cancel");
    expect(
      native.mock.calls.some(([command]) => command === "server_sync_publish"),
    ).toBe(false);
  });
  it("uses one active operation for duplicate run requests", async () => {
    const { facade, native } = fixture();
    const first = facade.cycle();
    const second = facade.cycle();
    expect(first).toBe(second);
    await first;
    expect(
      native.mock.calls.filter(
        ([command]) => command === "server_sync_prepare",
      ),
    ).toHaveLength(1);
  });
  it("retains the fence after an uncertain activation reply and confirms the same preparation", async () => {
    const { facade, native, fence } = fixture();
    const original = native.getMockImplementation()!;
    let lost = true;
    native.mockImplementation(async (command) => {
      if (command === "server_sync_activate" && lost) {
        lost = false;
        throw new Error("synthetic lost IPC reply");
      }
      return original(command);
    });
    await expect(facade.cycle()).rejects.toMatchObject({
      code: "activation-confirmation-pending",
    });
    expect(fence.release).not.toHaveBeenCalled();
    expect(facade.needsRefresh()).toBe(true);
    await expect(facade.cancel()).rejects.toMatchObject({
      code: "activation-confirmation-pending",
    });
    await expect(facade.cycle()).resolves.toEqual(result);
    expect(
      native.mock.calls.filter(
        ([command]) => command === "server_sync_prepare",
      ),
    ).toHaveLength(1);
    expect(
      native.mock.calls.filter(
        ([command]) => command === "server_sync_activate",
      ),
    ).toHaveLength(2);
    expect(fence.refreshCommittedWorkingSet).toHaveBeenCalledOnce();
    expect(fence.release).toHaveBeenCalledOnce();
  });
  it("claims the library only after library work outside the facade settles", async () => {
    const { runtime, native, trace } = fixture();
    const original = native.getMockImplementation()!;
    native.mockImplementation(async (command) =>
      command === "server_sync_status" ? ({ localRevision: 7 } as never) : original(command));
    let settle!: () => void;
    const awaitLibrary = vi.fn(() => new Promise<void>((resolve) => { settle = resolve; }));
    const facade = createServerSyncFacade({ runtime, invoke: native as never, awaitLibrary });
    const idle = () => new Promise((resolve) => setTimeout(resolve, 0));
    const config = { endpoint: "http://localhost", libraryId: "library", deviceId: "device", token: "token" };
    const claims = [
      [() => facade.cycle(), "server_sync_prepare"],
      [() => facade.bind(config), "server_sync_bind"],
      [() => facade.unbind(), "server_sync_unbind"],
      [() => facade.reconcile(), "server_sync_reconcile"],
    ] as const;
    for (const [claim, command] of claims) {
      const pending = claim();
      await idle();
      expect(trace).not.toContain(command);
      settle();
      await pending;
      expect(trace).toContain(command);
    }
    expect(awaitLibrary).toHaveBeenCalledTimes(claims.length);
  });
  it("stops a cycle cancelled while it waits for the library", async () => {
    const { runtime, native, trace } = fixture();
    let settle!: () => void;
    const facade = createServerSyncFacade({
      runtime,
      invoke: native as never,
      awaitLibrary: () => new Promise<void>((resolve) => { settle = resolve; }),
    });
    const cycle = facade.cycle();
    await facade.cancel();
    settle();
    await expect(cycle).rejects.toMatchObject({ code: "cancelled" });
    expect(trace).toEqual(["server_sync_cancel"]);
    expect(runtime.flushPendingData).not.toHaveBeenCalled();
  });
});

function heldFixture() {
  const trace: string[] = [];
  const physical = {
    revision: 7,
    refreshCommittedWorkingSet: vi.fn(async (revision: number): Promise<CommittedApplyOutcome> => {
      trace.push("refresh");
      return { kind: 'committed', revision, projection: 'applied' };
    }),
    release: vi.fn(() => {
      trace.push("release");
    }),
  };
  const fenced = async (): Promise<never> => {
    throw new Error("synthetic fenced mutation");
  };
  const runtime = {
    flushPendingData: vi.fn(fenced),
    capturePersistentMutationToken: vi.fn(fenced),
    acquireDestructiveReplacementFence: vi.fn(fenced),
    refreshActiveWorkingSetFromStore: vi.fn(fenced),
  };
  const native = vi.fn(async (command: string): Promise<unknown> => {
    trace.push(command);
    if (command === "server_sync_prepare") return {
      kind: "ready",
      preparationId: "prepared",
      localRevision: 7,
      head,
      appliedRecords: 1,
    };
    if (command === "server_sync_activate")
      return { revision: 8, pluginsChanged: false, devicePluginsChanged: false };
    if (command === "server_sync_publish") return result;
    return undefined;
  });
  const progress: string[] = [];
  const facade = createServerSyncFacade({
    runtime,
    invoke: native as never,
    onProgress: (phase) => progress.push(phase),
  });
  return { facade, native, runtime, physical, trace, progress, exit: retainableReplacementFence(physical) };
}
describe("server sync under a held exit fence", () => {
  it("applies and publishes without saving or fencing again", async () => {
    const { facade, runtime, physical, trace, progress, exit } = heldFixture();
    await expect(facade.cycle({}, exit)).resolves.toEqual(result);
    expect(runtime.flushPendingData).not.toHaveBeenCalled();
    expect(runtime.capturePersistentMutationToken).not.toHaveBeenCalled();
    expect(runtime.acquireDestructiveReplacementFence).not.toHaveBeenCalled();
    expect(trace).toEqual([
      "server_sync_prepare",
      "server_sync_activate",
      "refresh",
      "server_sync_publish",
    ]);
    expect(progress).toEqual(["preparing", "applying", "refreshing", "publishing"]);
    expect(physical.refreshCommittedWorkingSet).toHaveBeenCalledWith(8, undefined);
    exit.release();
    expect(physical.release).toHaveBeenCalledOnce();
  });
  it("keeps the fence past the exit until a pending activation is confirmed", async () => {
    const { facade, native, runtime, physical, exit } = heldFixture();
    const original = native.getMockImplementation()!;
    native.mockImplementationOnce(original).mockImplementationOnce(async () => {
      throw { code: "synthetic-lost-reply" };
    });
    await expect(facade.cycle({}, exit)).rejects.toMatchObject({
      code: "activation-confirmation-pending",
    });
    exit.release();
    expect(physical.release).not.toHaveBeenCalled();
    await expect(facade.cycle()).resolves.toEqual(result);
    expect(physical.refreshCommittedWorkingSet).toHaveBeenCalledWith(8, undefined);
    expect(physical.release).toHaveBeenCalledOnce();
    expect(runtime.acquireDestructiveReplacementFence).not.toHaveBeenCalled();
  });
});
