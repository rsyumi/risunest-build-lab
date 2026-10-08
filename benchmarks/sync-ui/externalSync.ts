import { SyncDriver, SyncStepError } from "./driver";
import { errorShape, guarded, type WebDavInput } from "./externalStorage";

type Seed = () => Promise<unknown>;

const missing = (step: string, message: string, detail: Record<string, unknown> = {}): never => {
  throw new SyncStepError("sync-result", step, message, detail);
};

type Invoke = (command: unknown, ...rest: unknown[]) => Promise<unknown>;

/** Records how each `external_lww_publish` call the product sends through the Tauri IPC entry ends. */
function observePublishes() {
  const calls: { ms: number; outcome: string }[] = [];
  const internals = (globalThis as { __TAURI_INTERNALS__?: { invoke: Invoke } }).__TAURI_INTERNALS__;
  if (!internals) return { calls, observed: false };
  const original = internals.invoke;
  try {
    internals.invoke = function (this: unknown, command: unknown, ...rest: unknown[]) {
      const result = original.call(this, command, ...rest);
      if (command !== "external_lww_publish") return result;
      const started = performance.now();
      const elapsed = () => Math.round(performance.now() - started);
      return Promise.resolve(result).then(
        (value) => { calls.push({ ms: elapsed(), outcome: "resolved" }); return value; },
        (error) => { calls.push({ ms: elapsed(), outcome: errorShape(error).kind ?? errorShape(error).code ?? "rejected" }); throw error; },
      );
    };
  } catch {
    return { calls, observed: false };
  }
  return { calls, observed: internals.invoke !== original };
}

const modules = async () => ({
  bridge: (await import("../../src/ts/storage/sync/external/bridge")).getExternalStorageBridge(),
  connection: await import("../../src/ts/storage/sync/external/connection"),
  production: await import("../../src/ts/storage/sync/external/production"),
  lww: await import("../../src/ts/storage/sync/external/lwwProduction"),
  registry: await import("../../src/ts/storage/sync/bindingRegistry"),
  native: (await import("../../src/ts/storage/sync/bindingNative")).createNativeSyncBindingBridge(),
  runtime: (await import("../../src/ts/storage/persistentDataRuntime.svelte")).getPersistentDataRuntime(),
});

async function withDeadline<T>(ms: number, run: Promise<T>, expired: () => never): Promise<T> {
  let timer: ReturnType<typeof setTimeout> | undefined;
  try {
    return await Promise.race([run, new Promise<never>((_, reject) => { timer = setTimeout(() => { try { expired(); } catch (error) { reject(error); } }, ms); })]);
  } finally {
    clearTimeout(timer);
  }
}

/** Publishes the way the sync button does and checks the product reported no failure and left nothing queued. */
async function publishNow(step: string, connectionId: string, observer: ReturnType<typeof observePublishes>) {
  const { lww, native, runtime } = await modules();
  const before = observer.calls.length;
  const deadline = Date.now() + 60_000;
  // After a relaunch the adapter appears once the product has read the connection list.
  for (;;) {
    try {
      await withDeadline(180_000, lww.requestExternalLwwNow(connectionId), () => missing(step, "the publish did not finish", { calls: observer.calls.slice(before) }));
      break;
    } catch (error) {
      if (error instanceof SyncStepError) throw error;
      if (error instanceof Error && error.message === "Sync binding transport is unavailable" && Date.now() < deadline) {
        await new Promise((resolve) => setTimeout(resolve, 500));
        continue;
      }
      missing(step, "the product refused the publish", { ...errorShape(error), calls: observer.calls.slice(before) });
    }
  }
  let failure: unknown;
  lww.subscribeExternalLwwFailures((failures) => { failure = failures.get(connectionId); })();
  if (failure !== undefined) missing(step, "the product reported a sync failure", errorShape(failure));
  const calls = observer.calls.slice(before);
  if (observer.observed && (!calls.length || calls.some((call) => call.outcome !== "resolved")))
    missing(step, "the publish call did not resolve", { calls });
  const state = await native.state();
  let queued = -1;
  await runtime.runStorageOnlyMutation(async () => {
    const page = await runtime.store.lwwReadOutbox!({ bindingAuthority: state.targetAuthority, requestId: crypto.randomUUID(), limit: "16" });
    queued = page.entries.length;
    return page.revision;
  });
  if (queued !== 0) missing(step, "local changes are still waiting to be published", { queued });
  return { publishCalls: calls, observed: observer.observed, queued };
}

/** Turns on sync for the connection with the sync switch's call, answering a confirmation as a person would. */
async function turnSyncOn(driver: SyncDriver, step: string, connectionId: string) {
  const { native, production, registry } = await modules();
  const { get } = await import("svelte/store");
  const { alertStore } = await import("../../src/ts/stores.svelte");
  const dialogs = { replacementShown: false, replacementGatedByCheckbox: null as boolean | null, previousFilesShown: false };
  const binding = guarded(step, () => registry.bindSyncTarget({ kind: "external", connectionId }));
  let settled = false;
  binding.then(() => { settled = true; }, () => { settled = true; });
  const deadline = Date.now() + 180_000;
  while (!settled) {
    if (alertStore.dialogVisible()) await driver.answerDialog(get(alertStore), "refuse", dialogs);
    if (Date.now() > deadline) missing(step, "turning on sync did not finish", dialogs);
    await new Promise((resolve) => setTimeout(resolve, 100));
  }
  const outcome = await binding;
  await guarded(step, () => production.refreshExternalStorageProductionState());
  if (outcome.kind !== "bound") missing(step, "turning on sync was cancelled");
  const state = await native.state();
  if (state.target.kind !== "external" || state.target.connectionId !== connectionId)
    missing(step, "the connection did not become the sync target", { target: state.target.kind });
  return { action: outcome.kind === "bound" ? outcome.action : null, target: state.target.kind, ...dialogs };
}

/**
 * Seeds an unbound profile, connects a new WebDAV sync repository, turns on sync so it becomes the
 * sync target, changes the last message and publishes it, then turns sync off and on again and
 * publishes once more.
 */
export async function webDavSyncPublish(driver: SyncDriver, input: {
  seed: Seed; platform: "macos" | "ios"; characterId: string; webdav: WebDavInput; first: string;
}) {
  await driver.step("sync-env", "profile", async () => {
    const binding = await driver.bindingTarget();
    if (binding !== "none") throw new SyncStepError("sync-env", "profile", "profile is already bound to a sync target", { binding });
    await driver.prepareProfile(input.seed);
    return { binding, seeded: true };
  });
  await driver.step("sync-result", "mount", () => driver.mountProduct());
  const observer = observePublishes();
  let connectionId = "";
  await driver.step("sync-result", "connected", async () => {
    const { bridge, connection, production } = await modules();
    const state = await guarded("connected", () => bridge.getState());
    if (!state.supported) missing("connected", "external storage is not supported in this build");
    const request = connection.buildPrepareConnectionRequest({
      providerId: "webdav",
      values: { endpoint: input.webdav.endpoint, accountId: input.webdav.accountId, root: input.webdav.root },
      platform: input.platform, mode: "create", purpose: "sync", acknowledgements: [],
    });
    await guarded("connected", () => bridge.validateSyncRoot(request.config));
    const prepared = await guarded("connected", () => bridge.prepareConnection(request));
    if (prepared.requiresOAuth || prepared.requiresFolderSelection)
      missing("connected", "WebDAV asked for OAuth or a folder selection", { requiresOAuth: prepared.requiresOAuth });
    const result = await guarded("connected", () =>
      bridge.commitConnection(prepared.preparationId, { kind: "webdav", password: input.webdav.password }));
    connectionId = result.connection.id;
    await guarded("connected", () => production.refreshExternalStorageProductionState());
    return {
      status: result.connection.status, strategy: result.connection.strategy, purpose: result.connection.purpose,
      recoveryKeyIssued: Boolean(result.recovery?.key), remoteVerified: prepared.endpoint.remoteVerified,
    };
  });
  await driver.step("sync-result", "sync-on", () => turnSyncOn(driver, "sync-on", connectionId));
  await driver.step("sync-result", "changed", async () => {
    await driver.editLastMessage(input.characterId, input.first, "external-sync-change");
    return { persisted: true };
  });
  await driver.step("sync-result", "published", () => publishNow("published", connectionId, observer));
  // Turning the switch off and on again in one session must bind again.
  await driver.step("sync-result", "sync-off", async () => {
    const { native, production, registry } = await modules();
    await guarded("sync-off", () => registry.unbindSyncTarget());
    await guarded("sync-off", () => production.refreshExternalStorageProductionState());
    const state = await native.state();
    if (state.target.kind !== "none") missing("sync-off", "sync stayed on after turning it off", { target: state.target.kind });
    return { target: state.target.kind };
  });
  await driver.step("sync-result", "sync-on-again", () => turnSyncOn(driver, "sync-on-again", connectionId));
  const published = await driver.step("sync-result", "republished", () => publishNow("republished", connectionId, observer));
  const location = await driver.step("sync-result", "marker-kept", async () => {
    const found = await driver.findMarker(input.first);
    if (!found.location) missing("marker-kept", "the published message is not in the library");
    return { characterId: found.location!.characterId, conversationId: found.location!.conversationId };
  });
  const shown = await driver.step("sync-result", "working-set", () =>
    driver.renderMarker({ characterId: location.characterId, conversationId: location.conversationId }, input.first));
  return { publishCalls: published.publishCalls, observed: published.observed, rendered: shown.rendered };
}

/** After a relaunch, sync is still on for the same connection, the change survived, and a second change publishes. */
export async function webDavSyncAfterRelaunch(driver: SyncDriver, input: { characterId: string; first: string; second: string }) {
  await driver.step("sync-env", "profile", async () => ({ binding: await driver.bindingTarget() }));
  await driver.step("sync-result", "mount", () => driver.mountProduct());
  const observer = observePublishes();
  let connectionId = "";
  await driver.step("sync-result", "sync-kept", async () => {
    const { bridge, native } = await modules();
    const state = await native.state();
    const connections = (await guarded("sync-kept", () => bridge.getState())).connections.filter((entry) => entry.purpose === "sync");
    if (state.target.kind !== "external" || connections.length !== 1 || state.target.connectionId !== connections[0].id)
      missing("sync-kept", "sync did not stay on for the WebDAV connection", { target: state.target.kind, connections: connections.length });
    connectionId = connections[0].id;
    return { target: state.target.kind, status: connections[0].status };
  });
  await driver.step("sync-result", "library-kept", async () => {
    if (!(await driver.findMarker(input.first)).found) missing("library-kept", "the published message did not survive the relaunch");
    return { found: true };
  });
  await driver.step("sync-result", "changed", async () => {
    await driver.editLastMessage(input.characterId, input.second, "external-sync-change-again");
    return { persisted: true };
  });
  const published = await driver.step("sync-result", "published", () => publishNow("published", connectionId, observer));
  return { publishCalls: published.publishCalls, observed: published.observed };
}
