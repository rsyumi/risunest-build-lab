import { SyncDriver, SyncStepError, redact } from "./driver";

/** External storage backup and restore through the product's own production paths, shared by the macOS and iOS harnesses. */
export interface WebDavInput {
  endpoint: string;
  accountId: string;
  password: string;
  root: string;
}

type Seed = () => Promise<unknown>;

const missing = (step: string, message: string, detail: Record<string, unknown> = {}): never => {
  throw new SyncStepError("sync-result", step, message, detail);
};

// The code and kind of a native error, never its message, which may carry an endpoint.
function errorShape(error: unknown) {
  if (!error || typeof error !== "object") return { error: typeof error };
  const value = error as { code?: unknown; kind?: unknown; reason?: unknown; name?: unknown };
  const text = (field: unknown) => typeof field === "string" ? redact(field).slice(0, 80) : undefined;
  return { code: text(value.code), kind: text(value.kind), reason: text(value.reason), name: text(value.name) };
}

async function guarded<T>(step: string, run: () => Promise<T>): Promise<T> {
  try {
    return await run();
  } catch (error) {
    if (error instanceof SyncStepError) throw error;
    return missing(step, "the product refused the operation", errorShape(error));
  }
}

const modules = async () => ({
  bridge: (await import("../../src/ts/storage/sync/external/bridge")).getExternalStorageBridge(),
  connection: await import("../../src/ts/storage/sync/external/connection"),
  production: await import("../../src/ts/storage/sync/external/production"),
  scope: await import("../../src/ts/storage/sync/external/restoreScope"),
});

/** Runs a manual backup the way the settings page does and returns its snapshot. */
async function backupNow(step: string, connectionId: string) {
  const { production } = await modules();
  const result = await guarded(step, () => production.requestExternalStorageNow(connectionId, "backup"));
  if (result.kind === "cancelled") return missing(step, "the backup was cancelled");
  if (result.kind === "blocked")
    return missing(step, "the backup did not complete", {
      reason: redact(result.reason), error: errorShape(result.error), jobState: result.job?.state ?? null,
    });
  const snapshotId = result.job.result?.snapshotId;
  if (!snapshotId) return missing(step, "the backup reported no snapshot", { jobState: result.job.state });
  return {
    snapshotId,
    summary: {
      jobState: result.job.state,
      counters: result.job.counters ?? null,
      completedBytes: result.job.completedBytes,
      completedItems: result.job.completedItems,
      publishedRevision: result.job.result?.publishedRevision ?? null,
    },
  };
}

async function historyEntry(step: string, connectionId: string, snapshotId: string) {
  const { bridge } = await modules();
  const page = await guarded(step, () => bridge.listHistory(connectionId));
  const item = page.items.find((entry) => entry.snapshotId === snapshotId);
  if (!item) return missing(step, "the backup is not in the history", { items: page.items.length });
  return {
    item,
    summary: {
      items: page.items.length,
      kind: item.kind,
      complete: item.complete,
      verified: item.verified,
      sameDevice: item.sameDevice,
      includedSections: item.includedSections,
    },
  };
}

async function markerState(driver: SyncDriver, present: string, absent: string, step: string) {
  const found = await driver.findMarker(present);
  const stale = await driver.findMarker(absent);
  if (!found.location) missing(step, "the expected message is not in the library", { characters: found.characters });
  if (stale.found) missing(step, "the replaced message is still in the library");
  return found.location!;
}

/**
 * Seeds an unbound profile, writes `before` into the last message, connects a new WebDAV backup
 * repository, backs up, replaces the message with `after`, restores the backup and checks that the
 * library and the open chat hold `before` again.
 */
export async function webDavBackupRestore(driver: SyncDriver, input: {
  seed: Seed; platform: "macos" | "ios"; characterId: string; webdav: WebDavInput; before: string; after: string;
}) {
  await driver.step("sync-env", "profile", async () => {
    const binding = await driver.bindingTarget();
    if (binding !== "none") throw new SyncStepError("sync-env", "profile", "profile is already bound to a sync target", { binding });
    await driver.prepareProfile(input.seed);
    return { binding, seeded: true };
  });
  await driver.step("sync-result", "mount", () => driver.mountProduct());
  await driver.step("sync-result", "marker-written", async () => {
    await driver.editLastMessage(input.characterId, input.before, "external-backup-before");
    return { persisted: true };
  });
  let connectionId = "";
  await driver.step("sync-result", "connected", async () => {
    const { bridge, connection, production } = await modules();
    const state = await guarded("connected", () => bridge.getState());
    if (!state.supported) missing("connected", "external storage is not supported in this build");
    const request = connection.buildPrepareConnectionRequest({
      providerId: "webdav",
      values: { endpoint: input.webdav.endpoint, accountId: input.webdav.accountId, root: input.webdav.root },
      platform: input.platform, mode: "create", purpose: "backup", acknowledgements: [],
    });
    const prepared = await guarded("connected", () => bridge.prepareConnection(request));
    if (prepared.requiresOAuth || prepared.requiresFolderSelection)
      missing("connected", "WebDAV asked for OAuth or a folder selection", { requiresOAuth: prepared.requiresOAuth });
    const result = await guarded("connected", () =>
      bridge.commitConnection(prepared.preparationId, { kind: "webdav", password: input.webdav.password }));
    connectionId = result.connection.id;
    await guarded("connected", () => production.refreshExternalStorageProductionState());
    return {
      status: result.connection.status, strategy: result.connection.strategy, mode: result.connection.mode,
      recoveryKeyIssued: Boolean(result.recovery?.key), remoteVerified: prepared.endpoint.remoteVerified,
      connectionsBefore: state.connections.length,
    };
  });
  let snapshotId = "";
  const backedUp = await driver.step("sync-result", "backed-up", async () => {
    const outcome = await backupNow("backed-up", connectionId);
    snapshotId = outcome.snapshotId;
    return outcome.summary;
  });
  let restorable: Awaited<ReturnType<typeof historyEntry>>["item"] | undefined;
  await driver.step("sync-result", "history", async () => {
    const entry = await historyEntry("history", connectionId, snapshotId);
    restorable = entry.item;
    return entry.summary;
  });
  await driver.step("sync-result", "changed", async () => {
    await driver.editLastMessage(input.characterId, input.after, "external-backup-after");
    await markerState(driver, input.after, input.before, "changed");
    return { persisted: true };
  });
  const restored = await driver.step("sync-result", "restored", async () => {
    const { bridge, production, scope } = await modules();
    const areas = scope.externalRestoreAreas(restorable!, []);
    // The request resolves once the library is replaced; the native job settles after the app adopts it.
    const job = await guarded("restored", () => production.requestExternalStorageRestore(connectionId, snapshotId, areas));
    const receivedRevision = job.result?.receivedRevision;
    if (job.applicationStarted !== true || receivedRevision === undefined)
      missing("restored", "the restore did not apply", { jobState: job.state, applicationStarted: job.applicationStarted ?? null, error: errorShape(job.error) });
    let settled = job;
    const deadline = Date.now() + 120_000;
    while (!["succeeded", "failed", "cancelled", "uncertain", "conflict"].includes(settled.state) && Date.now() < deadline) {
      await new Promise(resolve => setTimeout(resolve, 500));
      settled = await guarded("restored", () => bridge.getJob(job.id));
    }
    if (settled.state !== "succeeded")
      missing("restored", "the restore job did not settle as succeeded", { jobState: settled.state, error: errorShape(settled.error) });
    return { jobState: settled.state, areas, receivedRevision };
  });
  const location = await driver.step("sync-result", "library-restored", async () => {
    const found = await markerState(driver, input.before, input.after, "library-restored");
    return { characterId: found.characterId, conversationId: found.conversationId, markerInLastMessage: found.last };
  });
  const shown = await driver.step("sync-result", "working-set", () =>
    driver.renderMarker({ characterId: location.characterId, conversationId: location.conversationId }, input.before));
  return { backupBytes: backedUp.completedBytes, restoreAreas: restored.areas, rendered: shown.rendered };
}

/**
 * After a relaunch, the backup connection is still there and usable with its stored secret, and the
 * restored library survived: it backs up again and finds a second history entry.
 */
export async function webDavAfterRelaunch(driver: SyncDriver, input: { before: string }) {
  let connectionId = "";
  await driver.step("sync-env", "profile", async () => {
    const binding = await driver.bindingTarget();
    return { binding };
  });
  await driver.step("sync-result", "mount", () => driver.mountProduct());
  await driver.step("sync-result", "connection-kept", async () => {
    const { bridge } = await modules();
    const state = await guarded("connection-kept", () => bridge.getState());
    const kept = state.connections.filter((entry) => entry.providerId === "webdav");
    if (kept.length !== 1) missing("connection-kept", "the WebDAV connection did not survive the relaunch", { connections: state.connections.length });
    connectionId = kept[0].id;
    return { status: kept[0].status, lastBackup: Boolean(kept[0].lastBackupAtMs) };
  });
  await driver.step("sync-result", "library-kept", async () => {
    const found = await driver.findMarker(input.before);
    if (!found.found) missing("library-kept", "the restored message did not survive the relaunch");
    return { found: true };
  });
  const again = await driver.step("sync-result", "backed-up-again", async () => (await backupNow("backed-up-again", connectionId)).summary);
  const listed = await driver.step("sync-result", "history-after", async () => {
    const { bridge } = await modules();
    const page = await guarded("history-after", () => bridge.listHistory(connectionId));
    if (page.items.length < 2) missing("history-after", "the history does not hold both backups", { items: page.items.length });
    return { items: page.items.length };
  });
  return { backupBytes: again.completedBytes, historyItems: listed.items };
}
