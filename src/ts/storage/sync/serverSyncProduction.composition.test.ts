import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { ServerCycle, ServerStatus } from "./serverSync";

const state = vi.hoisted(() => ({
  desktop: true,
  finish: undefined as undefined | (() => void),
  cycle: vi.fn(),
  cancel: vi.fn(),
  invoke: vi.fn(async () => {}),
  revision: undefined as undefined | (() => void),
  statusFailures: 0,
}));
vi.mock("../../platform", () => ({ isTauri: true, get isTauriDesktop() { return state.desktop; } }));
vi.mock("@tauri-apps/api/core", () => ({ invoke: state.invoke }));
vi.mock("@tauri-apps/api/event", () => ({ listen: vi.fn(async () => () => {}) }));
vi.mock("../../mobileBackgroundTask", () => ({
  hasMobileBackgroundTasks: () => false,
  subscribeMobileBackgroundTasks: () => () => {},
  beginMobileBackgroundTask: async () => ({ progress: () => {}, dispose: async () => {}, signal: undefined }),
}));
vi.mock("../persistentDataRuntime.svelte", () => ({
  flushPendingData: vi.fn(), capturePersistentMutationToken: vi.fn(),
  acquireDestructiveReplacementFence: vi.fn(), refreshActiveWorkingSetFromStore: vi.fn(),
}));
vi.mock("../../plugins/pluginDeviceKeyspace", () => ({ invalidatePluginDeviceKeyspaces: vi.fn() }));
vi.mock("../persistentRevisionEvents", () => ({
  subscribeLocalPersistentRevision: (listener: () => void) => { state.revision = listener; return () => {}; },
}));
vi.mock("./serverSync", async original => ({
  ...(await original<object>()),
  createServerSyncFacade: () => ({
    status: async () => {
      if (state.statusFailures > 0) {
        state.statusFailures -= 1;
        throw new Error("synthetic store lock");
      }
      return status;
    },
    cycle: state.cycle,
    cancel: state.cancel,
    needsRefresh: () => false,
  }),
}));

const head = {
  libraryId: "library", epoch: "epoch", seq: "0", headId: "head", minRetainedSeq: "0",
  sections: {
    library: { stateId: "library", changedSeq: "0", gcFloor: "0" },
    hypa: { stateId: "hypa", changedSeq: "0", gcFloor: "0" },
    "local-plugins": { stateId: "plugins", changedSeq: "0", gcFloor: "0" },
  },
};
const status: ServerStatus = {
  localRevision: 0, configured: true, reconciling: false, endpoint: "https://sync.invalid",
  libraryId: "library", deviceId: "device", head, dirtyRecords: 0, pendingDeviceSections: false,
  fullScan: false, registrationRequired: false, operationPending: false,
};
const result: ServerCycle = {
  endpoint: status.endpoint!, phase: "idle", localRevision: 0, head,
  conflictCount: 0, conflicts: [], appliedRecords: 0, proposedRecords: 0,
};
const listeners: Array<[EventTarget, string, EventListenerOrEventListenerObject]> = [];
const visibility = (value: "hidden" | "visible") => {
  Object.defineProperty(document, "visibilityState", { configurable: true, value });
  document.dispatchEvent(new Event("visibilitychange"));
};
const connectivity = (online: boolean) => {
  Object.defineProperty(navigator, "onLine", { configurable: true, value: online });
  window.dispatchEvent(new Event(online ? "online" : "offline"));
};
beforeEach(() => {
  vi.resetModules();
  vi.useFakeTimers();
  vi.clearAllMocks();
  localStorage.clear();
  state.desktop = true;
  state.finish = undefined;
  state.statusFailures = 0;
  state.cycle.mockImplementation(() => new Promise<ServerCycle>(resolve => {
    state.finish = () => resolve(result);
  }));
  state.cancel.mockImplementation(async () => state.finish?.());
  visibility("visible");
  connectivity(true);
  for (const target of [window, document]) {
    const add = target.addEventListener.bind(target);
    vi.spyOn(target, "addEventListener").mockImplementation((type, listener, options) => {
      if (listener) listeners.push([target, type, listener]);
      add(type, listener, options);
    });
  }
});
afterEach(() => {
  for (const [target, type, listener] of listeners.splice(0)) target.removeEventListener(type, listener);
  vi.clearAllTimers();
  vi.useRealTimers();
  vi.restoreAllMocks();
});

describe("production sync scheduling composition", () => {
  it("recovers transient startup status without asking for another registration", async () => {
    state.statusFailures = 2;
    const { startServerSync, getServerSyncController } = await import("./serverSyncProduction");
    startServerSync();
    await vi.advanceTimersByTimeAsync(0);
    expect(getServerSyncController().snapshot().status).toBeUndefined();
    expect(state.cycle).not.toHaveBeenCalled();
    await vi.advanceTimersByTimeAsync(250);
    expect(getServerSyncController().snapshot().status?.configured).toBe(true);
    await vi.advanceTimersByTimeAsync(1);
    expect(state.cycle).toHaveBeenCalledOnce();
    state.finish!();
    await getServerSyncController().waitForIdle();
  });

  it("waits through generation and starts from its release edge without error backoff", async () => {
    const { doingChat } = await import("../../process/generationState");
    doingChat.set(true);
    const { startServerSync, getServerSyncController } = await import("./serverSyncProduction");
    startServerSync();
    state.revision!();
    await vi.advanceTimersByTimeAsync(60_000);
    expect(state.cycle).not.toHaveBeenCalled();
    expect(getServerSyncController().snapshot().error).toBe("");
    doingChat.set(false);
    await vi.advanceTimersByTimeAsync(0);
    expect(state.cycle).toHaveBeenCalledOnce();
    state.finish!();
    await getServerSyncController().waitForIdle();
  });

  it("keeps exactly one desktop attempt across hidden and offline transitions, then resumes local work", async () => {
    const { startServerSync, getServerSyncController } = await import("./serverSyncProduction");
    startServerSync();
    await vi.advanceTimersByTimeAsync(0);
    expect(state.cycle).toHaveBeenCalledOnce();
    visibility("hidden");
    connectivity(false);
    state.invoke.mockClear();
    connectivity(true);
    expect(state.invoke).not.toHaveBeenCalledWith("server_sync_events_start");
    visibility("visible");
    await vi.advanceTimersByTimeAsync(0);
    expect(state.cycle).toHaveBeenCalledOnce();
    expect(state.cancel).not.toHaveBeenCalled();
    state.finish!();
    await getServerSyncController().waitForIdle();
    state.revision!();
    await vi.advanceTimersByTimeAsync(500);
    expect(state.cycle).toHaveBeenCalledTimes(2);
    state.finish!();
    await getServerSyncController().waitForIdle();
  });

  it("settles a hidden mobile attempt without resuming while hidden", async () => {
    state.desktop = false;
    const { startServerSync, getServerSyncController } = await import("./serverSyncProduction");
    startServerSync();
    await vi.advanceTimersByTimeAsync(0);
    visibility("hidden");
    await getServerSyncController().waitForIdle();
    await vi.advanceTimersByTimeAsync(60_000);
    expect(state.cancel).toHaveBeenCalledOnce();
    expect(state.cycle).toHaveBeenCalledOnce();
    visibility("visible");
    await vi.advanceTimersByTimeAsync(0);
    expect(state.cycle).toHaveBeenCalledTimes(2);
    state.finish!();
    await getServerSyncController().waitForIdle();
  });

  it("retains the durable manual pause after restart and resumes only explicitly", async () => {
    const first = await import("./serverSyncProduction");
    await first.getServerSyncController().pause();
    expect(localStorage.getItem("risuNestServerSyncRestoreHold")).toBe("true");
    vi.resetModules();
    const restarted = await import("./serverSyncProduction");
    restarted.startServerSync();
    await vi.advanceTimersByTimeAsync(60_000);
    expect(state.cycle).not.toHaveBeenCalled();
    const exit = restarted.createServerSyncExitDrainAdapter();
    await expect(exit.drain({ revision: 1, libraryEpoch: "e", selectionEpoch: "s", selectionId: "server" }, new AbortController().signal))
      .resolves.toEqual({ kind: "complete" });
    expect(localStorage.getItem("risuNestServerSyncRestoreHold")).toBe("true");
    const explicit = restarted.getServerSyncController().synchronize();
    await vi.advanceTimersByTimeAsync(0);
    expect(state.cycle).toHaveBeenCalledOnce();
    state.finish!();
    await explicit;
    expect(localStorage.getItem("risuNestServerSyncRestoreHold")).toBeNull();
  });
});
