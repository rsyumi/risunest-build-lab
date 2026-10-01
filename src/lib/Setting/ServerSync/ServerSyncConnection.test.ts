import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { mount, tick, unmount } from "svelte";
import type { ServerSyncSnapshot } from "src/ts/storage/sync/serverSyncController";

const state = vi.hoisted(() => {
  let listener: ((snapshot: unknown) => void) | undefined;
  const trace: string[] = [];
  const controller = {
    snapshot: vi.fn(),
    subscribe: vi.fn((next: (snapshot: unknown) => void) => {
      listener = next;
      return () => {
        listener = undefined;
      };
    }),
    initialize: vi.fn(),
    ensureStatus: vi.fn(),
    bind: vi.fn(async (_config: unknown, residency?: "full" | "remote") => {
      trace.push("bind");
    }),
    reregister: vi.fn(async (_config: unknown, residency?: "full" | "remote") => {
      trace.push("reregister");
    }),
    synchronize: vi.fn(async () => {
      trace.push("synchronize");
    }),
    unbind: vi.fn(async (_config: unknown, residency?: "full" | "remote") => {  }),
    pause: vi.fn(async () => {}),
    waitForIdle: vi.fn(async () => {}),
  };
  const setPolicy = vi.fn(async (policy: string) => {
    trace.push(`policy:${policy}`);
    return { policy: policy as 'full' | 'remote', localBytes: 0, localObjects: 0, remoteBytes: 0, remoteObjects: 0, unavailableObjects: 0, evictedBytes: 0 };
  });
  return { controller, setPolicy, trace, emit: (value: unknown) => listener?.(value) };
});
vi.mock("src/ts/storage/sync/serverSyncProduction", () => ({
  getServerSyncController: () => state.controller,
  getServerSyncBackupInventory: vi.fn(async () => ({
    items: [],
    next: null,
    completeCount: 0,
    completeBytes: 0,
    incompleteCount: 0,
    incompleteBytes: 0,
    diskBytes: 0,
  })),
  getServerSyncCacheUsage: vi.fn(async () => ({
    totalBytes: 0,
    cacheBytes: 0,
    protectedBytes: 0,
    reclaimableBytes: 0,
    ledgerBytes: 0,
    databaseBytes: 0,
    blockedReason: null,
  })),
  cleanupServerSyncCache: vi.fn(),
  deleteServerSyncBackup: vi.fn(),
  listServerSyncBackups: vi.fn(async () => ({ items: [], next: null })),
  restoreServerSyncBackup: vi.fn(),
}));
vi.mock("src/ts/storage/sync/serverAssetResidency", () => ({
  getAssetResidencyStatus: vi.fn(async () => ({
    policy: "full",
    localBytes: 0,
    remoteBytes: 0,
    remoteObjects: 0,
    unavailableObjects: 0,
    evictedBytes: 0,
  })),
  setAssetResidencyPolicy: state.setPolicy,
  evictLocalAssets: vi.fn(),
  cancelAssetResidencyOperation: vi.fn(),
}));
vi.mock("src/lang", async () => ({
  language: (await import("src/lang/en")).languageEnglish,
}));
vi.mock("src/ts/alert", () => ({ alertConfirm: vi.fn() }));
vi.mock("src/ts/stores.svelte", async () => ({
  alertStore: (await import("svelte/store")).writable({ type: "none", msg: "" }),
}));
import { serverRegistrationInbox } from "src/ts/storage/sync/serverSyncRegistrationInbox";
import { receiveServerRegistration } from "src/ts/storage/sync/serverSyncRegistrationDispatch";
import { languageEnglish } from "src/lang/en";
import vector from "../../../../crates/sync-connect/tests/registration-vector.json";
import ServerSyncConnection from "./ServerSyncConnection.svelte";
import { getServerSyncBackupInventory, getServerSyncCacheUsage } from "src/ts/storage/sync/serverSyncProduction";
import { getAssetResidencyStatus } from "src/ts/storage/sync/serverAssetResidency";
import { isLibraryFileOperationReserved, reserveLibraryFileOperation, subscribeLibraryFileOperationReleased } from "src/ts/storage/libraryFileOperation";
vi.mock("src/ts/storage/persistentDataRuntime.svelte", () => ({ getPersistentDataRuntime: () => ({ store: { acquireRevision: async () => { throw new Error("expired"); } } }) }));

const text = languageEnglish.risuNest.serverSync;
let target: HTMLDivElement;
let component: ReturnType<typeof mount> | undefined;
const idle = (): ServerSyncSnapshot => ({
  running: false,
  paused: false,
  error: "",
  status: { configured: false } as ServerSyncSnapshot["status"],
});
const bound = (endpoint = "https://bound.test/", libraryId = "lib"): ServerSyncSnapshot => ({
  ...idle(),
  status: {
    localRevision: 1,
    reconciling: false,
    configured: true,
    endpoint,
    libraryId,
    deviceId: "device",
    head: null,
    dirtyRecords: 0,
    pendingDeviceSections: false,
    fullScan: false,
    registrationRequired: false,
    operationPending: false,
  },
});
const button = (label: string) =>
  [...target.querySelectorAll<HTMLButtonElement>("button")].find((b) =>
    b.textContent?.trim().startsWith(label),
  )!;
const manualInputs = () => [...target.querySelectorAll<HTMLInputElement>("form.fields input")];
function fillManual(values: string[]): void {
  manualInputs().forEach((input, index) => {
    input.value = values[index];
    input.dispatchEvent(new Event("input", { bubbles: true }));
  });
  target
    .querySelector("form.fields")!
    .dispatchEvent(new Event("submit", { bubbles: true, cancelable: true }));
}
function submitReview(): void {
  target
    .querySelector("form.review-form")!
    .dispatchEvent(new Event("submit", { bubbles: true, cancelable: true }));
}
beforeEach(() => {
  vi.clearAllMocks();
  state.trace.length = 0;
  serverRegistrationInbox.clear();
  state.controller.snapshot.mockReturnValue(idle());
  target = document.createElement("div");
  document.body.append(target);
});
afterEach(async () => {
  if (component) await unmount(component);
  component = undefined;
  target.remove();
});
describe("settings server connection", () => {
  it("ignores unchanged idle publications and refreshes once after an attempt", async () => {
    const snapshot = bound();
    state.controller.snapshot.mockReturnValue(snapshot);
    component = mount(ServerSyncConnection, { target });
    await vi.waitFor(() => expect(getServerSyncBackupInventory).toHaveBeenCalledTimes(1));
    for (let i = 0; i < 10; i++) { state.emit({ ...snapshot }); await tick(); }
    expect(getServerSyncBackupInventory).toHaveBeenCalledTimes(1);
    expect(getServerSyncCacheUsage).toHaveBeenCalledTimes(1);
    state.emit({ ...snapshot, running: true }); await tick();
    state.emit({ ...snapshot, running: false }); await tick();
    await vi.waitFor(() => expect(getServerSyncBackupInventory).toHaveBeenCalledTimes(2));
  });
  it("retries unknown status without requesting a new registration", async () => {
    state.controller.snapshot.mockReturnValue({ ...idle(), status: undefined, error: 'local-validation' });
    component = mount(ServerSyncConnection, { target }); await tick();
    expect(button(text.enterCode)).toBeUndefined();
    expect(target.textContent).toContain(text.statusUnknown);
    button(text.retryStatus).click();
    expect(state.controller.ensureStatus).toHaveBeenCalledOnce();
    state.emit(bound()); await tick();
    expect(button(text.syncNow)).toBeDefined();
  });
  it("preserves a newer child inventory when an older parent summary completes", async () => {
    const inventory = { items: [], next: null, completeCount: 9, completeBytes: 0, incompleteCount: 0, incompleteBytes: 0, diskBytes: 0 };
    let finish!: (value: typeof inventory) => void;
    vi.mocked(getServerSyncBackupInventory).mockImplementationOnce(() => new Promise((resolve) => { finish = resolve; }));
    vi.mocked(getServerSyncBackupInventory).mockResolvedValueOnce(inventory);
    component = mount(ServerSyncConnection, { target }); await tick();
    button(text.viewList).click(); await tick();
    await vi.waitFor(() => expect(target.textContent).toContain(text.backupCount.replace('{0}', '9')));
    finish({ ...inventory, completeCount: 1 }); await tick(); await tick();
    expect(target.textContent).toContain(text.backupCount.replace('{0}', '9'));
  });
  it("shows changed conflict identities and catches a refused choice", async () => {
    const snapshot = { ...bound(), conflictRefreshed: true, result: {
      phase: 'conflict', endpoint: 'https://bound.test/', localRevision: 1, head: null, conflictCount: 1,
      conflicts: ['plugin-storage:fixture-plugin'], appliedRecords: 0, proposedRecords: 0,
    } } as unknown as ServerSyncSnapshot;
    state.controller.snapshot.mockReturnValue(snapshot);
    state.controller.synchronize.mockRejectedValueOnce({ code: 'library-operation-busy' });
    component = mount(ServerSyncConnection, { target });
    await vi.waitFor(() => expect(target.textContent).toContain('fixture-plugin'));
    expect(target.textContent).toContain(text.conflictRefreshed);
    button(text.keepRemote).click();
    await vi.waitFor(() => expect(target.querySelector('[role="alert"]')?.textContent).toContain(text.busyHelp));
  });
  it("renders synchronization admission refusal inline without an unhandled rejection", async () => {
    state.controller.snapshot.mockReturnValue(bound());
    state.controller.synchronize.mockRejectedValueOnce({ code: 'library-operation-busy' });
    component = mount(ServerSyncConnection, { target }); await tick();
    button(text.syncNow).click();
    await vi.waitFor(() => expect(target.querySelector('[role="alert"]')?.textContent).toContain(text.busyHelp));
  });
  it('keeps Pause enabled while the first connection cycle is active', async () => {
    let settle!: () => void;
    state.controller.synchronize.mockImplementationOnce(() => new Promise<void>((resolve) => { settle = resolve; }));
    component = mount(ServerSyncConnection, { target }); await tick();
    button(text.enterCode).click(); await tick();
    button(text.manualEntry).click(); await tick();
    fillManual(['https://bound.test/', 'lib', 'device', 'a'.repeat(64)]); await tick();
    submitReview(); await tick(); await tick();
    state.emit({ ...bound(), running: true, connecting: false }); await tick();
    expect(button(text.pause).disabled).toBe(false);
    button(text.pause).click(); await tick();
    expect(state.controller.pause).toHaveBeenCalledOnce();
    settle(); await tick();
  });
  it("resumes Full hydration, reserves admission and disables neighboring actions until settlement", async () => {
    state.controller.snapshot.mockReturnValue(bound());
    const status = { policy: 'full' as const, localBytes: 0, localObjects: 0, remoteBytes: 9, remoteObjects: 1, unavailableObjects: 0, evictedBytes: 0 };
    vi.mocked(getAssetResidencyStatus).mockResolvedValueOnce(status);
    let finish!: () => void;
    state.setPolicy.mockImplementationOnce(() => new Promise<Awaited<ReturnType<typeof state.setPolicy>>>((resolve) => { finish = () => resolve({ ...status, remoteObjects: 0 }); }));
    const released = vi.fn();
    const stop = subscribeLibraryFileOperationReleased(released);
    component = mount(ServerSyncConnection, { target });
    await vi.waitFor(() => expect(button(text.residency.download)).toBeDefined());
    button(text.residency.download).click(); await tick();
    expect(state.setPolicy).toHaveBeenCalledWith('full');
    expect(isLibraryFileOperationReserved()).toBe(true);
    expect(button(text.syncNow).disabled).toBe(true);
    expect(button(text.disconnect).disabled).toBe(true);
    expect(button(text.register).disabled).toBe(true);
    finish();
    await vi.waitFor(() => expect(isLibraryFileOperationReserved()).toBe(false));
    expect(released).toHaveBeenCalledOnce();
    stop();
  });
  it('explains a residency reservation refusal without starting native work', async () => {
    state.controller.snapshot.mockReturnValue(bound());
    vi.mocked(getAssetResidencyStatus).mockResolvedValueOnce({ policy: 'full', localBytes: 0, remoteBytes: 1, remoteObjects: 1, unavailableObjects: 0, evictedBytes: 0 });
    component = mount(ServerSyncConnection, { target });
    await vi.waitFor(() => expect(button(text.residency.download)).toBeDefined());
    const release = reserveLibraryFileOperation();
    try {
      button(text.residency.download).click();
      await vi.waitFor(() => expect(target.textContent).toContain(text.busyHelp));
      expect(state.setPolicy).not.toHaveBeenCalled();
      expect(isLibraryFileOperationReserved()).toBe(true);
    } finally { release(); }
  });
  it("separates comparison, receiving and application while naming backup metadata", async () => {
    const snapshot = { ...bound(), running: true, progress: "preparing" as const,
      cycleItems: { done: 0, total: 230, activity: "downloadingBackupMetadata" as const, processed: 1200, expected: 0 } };
    state.controller.snapshot.mockReturnValue(snapshot);
    component = mount(ServerSyncConnection, { target });
    await tick();
    expect(target.querySelector('[aria-current="step"]')?.textContent).toContain(text.progress.downloading);
    expect(target.textContent).toContain(`${text.activity.downloadingBackupMetadata} · 1,200`);
    const stages = [...target.querySelectorAll("ol li")].map((item) => item.textContent?.trim());
    expect(stages.findIndex((label) => label?.includes(text.progress.preparing)))
      .toBeLessThan(stages.findIndex((label) => label?.includes(text.progress.downloading)));
    expect(stages.findIndex((label) => label?.includes(text.progress.downloading)))
      .toBeLessThan(stages.findIndex((label) => label?.includes(text.progress.applying)));
    state.emit({ ...snapshot, progress: "applying" });
    await tick();
    expect(target.querySelector('[aria-current="step"]')?.textContent).toContain(text.progress.applying);
    expect(target.textContent).not.toContain(text.activity.downloadingBackupMetadata);
  });

  it("clears an old action error when a new sync attempt starts", async () => {
    const snapshot = bound();
    state.controller.snapshot.mockReturnValue(snapshot);
    state.controller.unbind.mockRejectedValueOnce({ code: "library-operation-busy" });
    component = mount(ServerSyncConnection, { target });
    await tick();
    button(text.disconnect).click();
    await vi.waitFor(() => expect(target.textContent).toContain("library-operation-busy"));
    snapshot.attemptId = 1;
    snapshot.running = true;
    state.emit({ ...snapshot });
    await tick();
    expect(target.textContent).not.toContain("library-operation-busy");
  });
  it("prefills public navigation in the manual entry without binding or initializing an engine", async () => {
    component = mount(ServerSyncConnection, {
      target,
      props: {
        origin: "deep-link",
        initialNavigation: {
          endpoint: "https://example.test/",
          libraryId: "lib",
        },
      },
    });
    await tick();
    expect(manualInputs().map((input) => input.value)).toEqual([
      "https://example.test/",
      "lib",
      "",
      "",
    ]);
    expect(button(text.manualEntry).getAttribute("aria-expanded")).toBe("true");
    expect(state.controller.bind).not.toHaveBeenCalled();
    expect(state.controller.initialize).not.toHaveBeenCalled();
    expect(state.controller.synchronize).not.toHaveBeenCalled();
  });
  it("keeps an existing identity when another endpoint arrives", async () => {
    state.controller.snapshot.mockReturnValue(bound("https://bound.test/", "bound"));
    component = mount(ServerSyncConnection, {
      target,
      props: {
        initialNavigation: {
          endpoint: "https://other.test/",
          libraryId: "other",
        },
      },
    });
    await tick();
    expect(target.textContent).toContain("https://bound.test/");
    expect(target.textContent).not.toContain("https://other.test/");
    expect(target.querySelector("form")).toBeNull();
    expect(state.controller.unbind).not.toHaveBeenCalled();
    expect(state.controller.bind).not.toHaveBeenCalled();
    expect(state.controller.reregister).not.toHaveBeenCalled();
  });
  it("binds only after the check is confirmed, stores the policy before the first sync, and drops the token", async () => {
    component = mount(ServerSyncConnection, { target });
    await tick();
    fillManual(["https://example.test", "lib", "device", "a".repeat(64)]);
    await tick();
    expect(state.controller.bind).not.toHaveBeenCalled();
    expect(target.querySelector("dl.review")!.textContent).toContain("https://example.test/");
    expect(target.textContent).not.toContain("a".repeat(64));
    target
      .querySelector<HTMLButtonElement>('[role="radio"]:nth-of-type(2)')!
      .click();
    await tick();
    submitReview();
    await vi.waitFor(() =>
      expect(state.controller.synchronize).toHaveBeenCalledOnce(),
    );
    expect(state.controller.bind).toHaveBeenCalledExactlyOnceWith({
      endpoint: "https://example.test/",
      libraryId: "lib",
      deviceId: "device",
      token: "a".repeat(64),
    }, "remote");
    expect(state.trace).toEqual(["bind", "synchronize"]);
    await vi.waitFor(() => expect(target.querySelector("dl.review")).toBeNull());
    expect(
      target.querySelector<HTMLInputElement>('form input[type="password"]')!
        .value,
    ).toBe("");
  });
  it("keeps controls busy until an explicit pause has settled", async () => {
    let settle!: () => void;
    state.controller.waitForIdle.mockImplementationOnce(
      () =>
        new Promise<void>((resolve) => {
          settle = resolve;
        }),
    );
    state.controller.snapshot.mockReturnValue(bound());
    component = mount(ServerSyncConnection, { target });
    await tick();
    const pause = button(text.pause);
    pause.click();
    await vi.waitFor(() =>
      expect(state.controller.waitForIdle).toHaveBeenCalledOnce(),
    );
    await tick();
    expect(pause.disabled).toBe(true);
    settle();
    await vi.waitFor(() => expect(pause.disabled).toBe(false));
    expect(state.controller.pause).toHaveBeenCalledOnce();
  });
  it("unsubscribes on leaving without cancelling the installation's operation", async () => {
    state.controller.snapshot.mockReturnValue({ ...idle(), running: true });
    component = mount(ServerSyncConnection, { target });
    await tick();
    await unmount(component);
    component = undefined;
    expect(state.controller.pause).not.toHaveBeenCalled();
  });
  it("shows the stage, the counters and a retryable transfer failure while synchronization continues", async () => {
    component = mount(ServerSyncConnection, { target });
    await tick();
    state.emit({
      ...bound(),
      running: true,
      progress: "applying",
      cycleItems: { done: 12, total: 48 },
      verifiedBytes: String(2 * 1024 * 1024),
      retryableFailure: "corrupt-chunk",
    });
    await tick();
    expect(target.textContent).toContain(`${text.running}: (corrupt-chunk)`);
    const bar = target.querySelector("[data-setting-progress]")!;
    expect(bar.textContent).toContain(`${text.progress.applying} · 12 / 48`);
    expect(bar.textContent).toContain("25%");
    expect(target.textContent).toContain("2.0 MiB");
    expect(target.textContent).not.toContain("Sync could not finish");
    expect(button(text.syncNow).disabled).toBe(true);
    expect(button(text.disconnect).disabled).toBe(true);
  });
  it("re-registers through its row without binding a second server", async () => {
    state.controller.snapshot.mockReturnValue(bound("https://bound.test/", "bound"));
    component = mount(ServerSyncConnection, { target });
    await tick();
    button(text.register).click();
    await tick();
    expect(manualInputs().map((input) => input.value)).toEqual([
      "https://bound.test/",
      "bound",
      "",
      "",
    ]);
    fillManual(["https://bound.test/", "bound", "device2", "b".repeat(64)]);
    await tick();
    expect(button(text.reregister)).toBeDefined();
    submitReview();
    await vi.waitFor(() =>
      expect(state.controller.reregister).toHaveBeenCalledExactlyOnceWith({
        endpoint: "https://bound.test/",
        libraryId: "bound",
        deviceId: "device2",
        token: "b".repeat(64),
      }, "full"),
    );
    expect(state.controller.bind).not.toHaveBeenCalled();
    await vi.waitFor(() =>
      expect(state.trace).toEqual(["reregister", "synchronize"]),
    );
  });
});

it("receives a cold registration after mounting, shows the check without secrets, and discards them after binding", async () => {
  await receiveServerRegistration(vector.uri, () => {
    component = mount(ServerSyncConnection, {
      target,
      props: { initialNavigation: {} },
    });
  });
  await tick();
  const review = target.querySelector("dl.review")!;
  expect(review.textContent).toContain("https://sync.example/base/");
  expect(review.textContent).toContain(vector.registration.libraryId);
  expect(review.textContent).toContain(vector.registration.deviceId);
  expect(review.textContent).toContain(vector.registration.directory.baseUrl);
  expect(target.textContent).not.toContain(vector.registration.token);
  expect(target.textContent).not.toContain(vector.registration.directory.key);
  expect(state.controller.bind).not.toHaveBeenCalled();
  submitReview();
  await vi.waitFor(() =>
    expect(state.controller.bind).toHaveBeenCalledExactlyOnceWith({
      ...vector.registration,
      endpoint: "https://sync.example/base/",
    }, "full"),
  );
  await vi.waitFor(() => expect(target.querySelector("dl.review")).toBeNull());
  expect(target.textContent).not.toContain(
    vector.registration.directory.baseUrl,
  );
  expect(serverRegistrationInbox.take()).toBeUndefined();
});
it("clears an imported credential without connecting and accepts a deliberate reimport", async () => {
  component = mount(ServerSyncConnection, { target });
  await tick();
  await receiveServerRegistration(vector.uri, () => {});
  await tick();
  expect(target.querySelector("dl.review")).not.toBeNull();
  button(text.otherCode).click();
  await tick();
  expect(target.querySelector("dl.review")).toBeNull();
  expect(manualInputs().map((input) => input.value)).toEqual(["", "", "", ""]);
  expect(target.textContent).not.toContain(
    vector.registration.directory.baseUrl,
  );
  expect(state.controller.bind).not.toHaveBeenCalled();
  expect(await receiveServerRegistration(vector.uri, () => {})).toBe(true);
});
it("rejects OS registration for an existing identity without an implicit replacement", async () => {
  state.controller.snapshot.mockReturnValue(bound("https://bound.test/", "bound"));
  component = mount(ServerSyncConnection, { target });
  await tick();
  await receiveServerRegistration(vector.uri, () => {});
  await tick();
  expect(target.textContent).toContain("https://bound.test/");
  expect(target.textContent).toContain("Disconnect the current server");
  expect(state.controller.bind).not.toHaveBeenCalled();
  expect(state.controller.reregister).not.toHaveBeenCalled();
  expect(serverRegistrationInbox.take()).toBeUndefined();
});

it("retains cold-start credentials until local startup mounts the public destination", async () => {
  await receiveServerRegistration(vector.uri, () => {});
  component = mount(ServerSyncConnection, {
    target,
    props: { initialNavigation: {} },
  });
  await tick();
  await tick();
  const review = target.querySelector("dl.review")!;
  expect(review).not.toBeNull();
  expect(review.textContent).toContain(vector.registration.libraryId);
  expect(review.textContent).toContain(vector.registration.deviceId);
  expect(target.textContent).not.toContain(vector.registration.token);
  expect(state.controller.bind).not.toHaveBeenCalled();
});
