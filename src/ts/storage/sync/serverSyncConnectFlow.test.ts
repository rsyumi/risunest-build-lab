import { describe, expect, it, vi } from "vitest";
import { languageEnglish } from "src/lang/en";
import type { ServerSyncSnapshot } from "./serverSyncController";
import {
  connectServerSync,
  serverSyncErrorHelp,
  serverSyncHostLabel,
  serverSyncProgressView,
  serverSyncStatus,
} from "./serverSyncConnectFlow";

const text = languageEnglish.risuNest.serverSync;
const config = {
  endpoint: "https://sync.example/base/",
  libraryId: "library",
  deviceId: "device",
  token: "a".repeat(64),
};
const idle = (): ServerSyncSnapshot => ({ running: false, paused: false, error: "" });
const bound = (): ServerSyncSnapshot => ({
  ...idle(),
  status: {
    localRevision: 3,
    reconciling: false,
    configured: true,
    endpoint: config.endpoint,
    libraryId: "library",
    deviceId: "device",
    head: null,
    dirtyRecords: 37,
    pendingDeviceSections: false,
    fullScan: false,
    registrationRequired: false,
    operationPending: false,
  },
});

describe("connecting a sync server", () => {
  it("binds with the durable asset policy before running the first sync", async () => {
    const trace: string[] = [];
    const controller = {
      bind: vi.fn(async (_config: unknown, residency?: "full" | "remote") => {
        trace.push("bind");
      }),
      reregister: vi.fn(async (_config: unknown, residency?: "full" | "remote") => {
        trace.push("reregister");
      }),
      synchronize: vi.fn(async () => {
        trace.push("synchronize");
      }),
    };
    await connectServerSync(controller, { config, residency: "remote" });
    expect(trace).toEqual(["bind", "synchronize"]);
    expect(controller.bind).toHaveBeenCalledWith(config, "remote");
  });
  it("re-registers instead of binding when replacing the device credentials", async () => {
    const controller = {
      bind: vi.fn(async (_config: unknown, residency?: "full" | "remote") => {  }),
      reregister: vi.fn(async (_config: unknown, residency?: "full" | "remote") => {  }),
      synchronize: vi.fn(async () => {}),
    };
    await connectServerSync(controller, {
      config,
      residency: "full",
      replacing: true,
    });
    expect(controller.reregister).toHaveBeenCalledWith(config, "full");
    expect(controller.bind).not.toHaveBeenCalled();
    expect(controller.synchronize).toHaveBeenCalledOnce();
  });
  it("does not sync when binding fails", async () => {
    const controller = {
      bind: vi.fn(async (_config: unknown, residency?: "full" | "remote") => {
        throw { code: "unauthorized" };
      }),
      reregister: vi.fn(async (_config: unknown, residency?: "full" | "remote") => {  }),
      synchronize: vi.fn(async () => {}),
    };
    await expect(
      connectServerSync(controller, { config, residency: "full" }),
    ).rejects.toMatchObject({ code: "unauthorized" });
    expect(controller.synchronize).not.toHaveBeenCalled();
  });
});

describe("status and error copy", () => {
  it.each(['unauthorized', 'forbidden', 'media-device-revoked', 'new-device-registration-required', 'device-identity-mismatch'])("explains %s without retry or software upgrade advice", (code) => {
    expect(serverSyncErrorHelp(code, text, false)).toBe(text.registrationRefusedHelp);
  });
  it("explains disconnect prerequisites and registration revocation", () => {
    expect(serverSyncErrorHelp('download-all-assets-before-unbind', text, false)).toBe(text.downloadBeforeDisconnect);
    expect(serverSyncErrorHelp('revoke-previous-device-first', text, false)).toBe(text.revokeBeforeRegistration);
    expect(serverSyncErrorHelp('local-storage-full', text, false)).toBe(text.storageFullHelp);
  });
  it("prioritizes confirmed disconnection over inactive binding pause", () => {
    expect(serverSyncStatus({ ...bound(), paused: true, status: { ...bound().status!, configured: false } }, text)).toEqual({ label: text.disconnected, tone: 'idle' });
    expect(serverSyncStatus({ ...bound(), error: 'unauthorized', errorRetryable: false }, text)).toEqual({ label: text.registrationRequired, tone: 'attention' });
    expect(serverSyncStatus({ ...bound(), error: 'generation-active' }, text)).toEqual({ label: text.waitingForLocal, tone: 'connected' });
  });
  it("ranks attention states above activity and activity above the connection", () => {
    expect(serverSyncStatus(idle(), text)).toEqual({
      label: text.statusUnknown,
      tone: "attention",
    });
    expect(serverSyncStatus(bound(), text)).toEqual({
      label: text.ready,
      tone: "connected",
    });
    expect(
      serverSyncStatus({ ...bound(), running: true, progress: "applying" }, text),
    ).toEqual({ label: text.progress.applying, tone: "working" });
    expect(
      serverSyncStatus(
        {
          ...bound(),
          running: true,
          result: {
            endpoint: config.endpoint,
            phase: "conflict",
            localRevision: 3,
            head: {
              libraryId: "library",
              epoch: "e",
              seq: "1",
              headId: "h",
              minRetainedSeq: "0",
              sections: {
                hypa: { stateId: "hypa-state", changedSeq: "0", gcFloor: "0" },
                library: { stateId: "library-state", changedSeq: "0", gcFloor: "0" },
                "local-plugins": { stateId: "plugins-state", changedSeq: "0", gcFloor: "0" },
              },
            },
            conflictCount: 2,
            conflicts: [],
            appliedRecords: 0,
            proposedRecords: 0,
          },
        },
        text,
      ),
    ).toEqual({ label: text.conflict, tone: "attention" });
    expect(
      serverSyncStatus(bound(), text, "committed-refresh-pending"),
    ).toEqual({ label: text.refreshPending, tone: "attention" });
    expect(serverSyncStatus({ ...bound(), paused: true }, text)).toEqual({
      label: text.paused,
      tone: "paused",
    });
  });
  it("explains refresh and credential failures specifically and everything else generally", () => {
    expect(serverSyncErrorHelp("committed-refresh-pending", text)).toBe(
      text.refreshHelp,
    );
    expect(serverSyncErrorHelp("activation-confirmation-pending", text)).toBe(
      text.activationHelp,
    );
    expect(serverSyncErrorHelp("device-credential-unavailable", text)).toBe(
      text.credentialUnavailable,
    );
    expect(serverSyncErrorHelp("server-unreachable", text)).toBe(text.errorHelp);
    expect(serverSyncErrorHelp("library-operation-busy", text)).toBe(text.busyHelp);
  });
  it("names the stopped state and the mismatched versions", () => {
    const blocked: ServerSyncSnapshot = {
      ...bound(),
      error: "invalid-control-schema",
      errorRetryable: false,
    };
    expect(serverSyncStatus(blocked, text)).toEqual({
      label: text.blocked,
      tone: "attention",
    });
    expect(serverSyncErrorHelp("invalid-control-schema", text, false)).toBe(
      text.blockedHelp,
    );
    expect(serverSyncErrorHelp("invalid-control-schema", text)).toBe(
      text.errorHelp,
    );
    const incompatible: ServerSyncSnapshot = {
      ...bound(),
      error: "server-incompatible",
      errorRetryable: false,
    };
    expect(serverSyncStatus(incompatible, text)).toEqual({
      label: text.incompatible,
      tone: "attention",
    });
    expect(serverSyncErrorHelp("server-incompatible", text, false)).toBe(
      text.incompatibleHelp,
    );
  });
  it("shows the address without its scheme", () => {
    expect(serverSyncHostLabel("https://sync.example/base/")).toBe(
      "sync.example/base",
    );
    expect(serverSyncHostLabel("not a url")).toBe("not a url");
  });
});

describe("progress view", () => {
  it("shows the checkpoint total for backup metadata before the next batch finishes", () => {
    const view = serverSyncProgressView({ ...bound(), running: true, progress: "preparing",
      cycleItems: { done: 0, total: 230, activity: "downloadingBackupMetadata", processed: 256, expected: 267 } }, text, 0);
    expect(view.current).toBe(`${text.activity.downloadingBackupMetadata} · 256 / 267`);
    expect(view.percent).toBe(96);
    expect(view.counters.find((counter) => counter.key === "items")?.value).toBe("256 / 267");
    expect(view.stages.find((stage) => stage.state === "active")?.stage).toBe("downloading");
  });
  it.each(["downloadingMetadata", "downloadingBackupMetadata", "downloading", "syncingSections"] as const)("shows %s counts independently of upload work", (activity) => {
    const view = serverSyncProgressView({ ...bound(), running: true, progress: "preparing",
      cycleItems: { done: 0, total: 200, activity, processed: 25, expected: 100 } }, text, 0);
    expect(view.current).toBe(`${text.activity[activity]} · 25 / 100`);
    expect(view.percent).toBe(25);
    expect(view.stages.map((stage) => stage.stage)).toEqual([
      "saving", "preparing", "downloading", "applying", "refreshing", "publishing",
    ]);
    expect(view.stages.find((stage) => stage.state === "active")?.stage).toBe("downloading");
    expect(view.counters.find((counter) => counter.key === "items")?.value).toBe("25 / 100");
    const unknown = serverSyncProgressView({ ...bound(), running: true, progress: "preparing",
      cycleItems: { done: 200, total: 200, activity, processed: 25, expected: 0 } }, text, 0);
    expect(unknown.current).toBe(`${text.activity[activity]} · 25`);
    expect(unknown.percent).toBeNull();
  });
  it("keeps backup preparation in the receive stage and ignores stale activity after activation", () => {
    const snapshot = { ...bound(), running: true, progress: "preparing" as const,
      cycleItems: { done: 0, total: 230, activity: "preserving" as const, processed: 1200, expected: 0 } };
    const view = serverSyncProgressView(snapshot, text, 0);
    expect(view.current).toBe(`${text.activity.preserving} · 1,200`);
    expect(view.percent).toBeNull();
    expect(view.stages.find((stage) => stage.state === "active")?.stage).toBe("downloading");
    for (const progress of ["applying", "refreshing", "publishing"] as const) {
      const next = serverSyncProgressView({ ...snapshot, progress }, text, 0);
      expect(next.stages.find((stage) => stage.state === "active")?.stage).toBe(progress);
    }
  });
  it("shows backend preparation counts and server waiting without claiming completion", () => {
    const snapshot = { ...bound(), running: true, progress: "preparing" as const,
      attemptStartedAt: 0,
      cycleItems: { done: 0, total: 0, activity: "preparing" as const, processed: 17, expected: 100 } };
    const view = serverSyncProgressView(snapshot, text, 15_000);
    expect(view.current).toBe(`${text.activity.preparing} · 17 / 100`);
    expect(view.elapsed).toBe(`${text.elapsed} 00:15`);
    expect(view.percent).toBeNull();
    const waiting = serverSyncProgressView({ ...snapshot, progress: "publishing",
      cycleItems: { done: 100, total: 100, activity: "confirming", processed: 0, expected: 0 } }, text, 15_000);
    expect(waiting.current).toBe(text.activity.confirming);
    expect(waiting.percent).toBeNull();
  });
  it("names the verification pass that sends no bytes", () => {
    const view = serverSyncProgressView(
      {
        ...bound(),
        running: true,
        progress: "publishing",
        cycleItems: {
          done: 0,
          total: 11_618,
          activity: "verifying",
          processed: 4_096,
          expected: 11_618,
        },
      },
      text,
      0,
    );
    expect(view.current).toBe(`${text.activity.verifying} · 4,096 / 11,618`);
  });
  it("is indeterminate until native reports the cycle's record count", () => {
    const view = serverSyncProgressView(
      { ...bound(), running: true, progress: "preparing", attemptStartedAt: 1000 },
      text,
      43_000,
    );
    expect(view.percent).toBeNull();
    expect(view.current).toBe(text.progress.preparing);
    expect(view.elapsed).toBe(`${text.elapsed} 00:42`);
    expect(view.stages.map((stage) => stage.state)).toEqual([
      "done",
      "active",
      "pending",
      "pending",
      "pending",
      "pending",
    ]);
    expect(view.counters.map((counter) => counter.value)).toEqual([
      "-",
      "-",
      "-",
      "37",
    ]);
  });
  it("derives the bar, the stage detail and the four tiles from the snapshot", () => {
    const view = serverSyncProgressView(
      {
        ...bound(),
        running: true,
        progress: "applying",
        verifiedBytes: String(13 * 1024 * 1024),
        bytesPerSecond: 1.8 * 1024 * 1024,
        cycleItems: { done: 1240, total: 3512 },
        attemptStartedAt: 0,
      },
      text,
      5_000,
    );
    expect(view.percent).toBe(35);
    expect(view.current).toBe(`${text.progress.applying} · 1,240 / 3,512`);
    expect(view.stages[1]).toMatchObject({ state: "done", detail: "3,512 items" });
    expect(view.stages[2]).toMatchObject({ stage: "downloading", state: "done" });
    expect(view.stages[3]).toMatchObject({ state: "active", detail: "1,240 / 3,512" });
    expect(view.stages[5]).toMatchObject({ state: "pending", detail: "" });
    expect(view.counters).toEqual([
      { key: "bytes", label: text.verifiedBytes, value: "13.0 MiB" },
      { key: "rate", label: text.transferRate, value: "1.8 MiB/s" },
      { key: "items", label: text.progressItems, value: "1,240 / 3,512" },
      { key: "pending", label: text.pendingChanges, value: "37" },
    ]);
  });
  it("counts the changes to send while the first full comparison is pending", () => {
    const snapshot = bound();
    snapshot.status!.fullScan = true;
    const view = serverSyncProgressView({ ...snapshot, running: true }, text, 0);
    expect(view.counters[3].value).toBe("37");
    expect(view.elapsed).toBe("");
    expect(serverSyncStatus(snapshot, text)).toEqual({
      label: text.initialScan,
      tone: "connected",
    });
  });
});
