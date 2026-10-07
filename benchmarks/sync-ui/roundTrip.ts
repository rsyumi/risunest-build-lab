import { SyncDriver, SyncStepError } from "./driver";

/** The device phases of a Sync round trip, shared by the macOS and iOS harnesses. */
type Seed = () => Promise<unknown>;

const missing = (step: string, message: string, detail: Record<string, unknown> = {}): never => {
  throw new SyncStepError("sync-result", step, message, detail);
};

async function unboundProfile(driver: SyncDriver, seed: Seed) {
  await driver.step("sync-env", "profile", async () => {
    const binding = await driver.bindingTarget();
    if (binding !== "none") throw new SyncStepError("sync-env", "profile", "profile is already bound to a sync target", { binding });
    await driver.prepareProfile(seed);
    return { binding, seeded: true };
  });
}

async function markerAbsent(driver: SyncDriver, marker: string) {
  await driver.step("sync-result", "marker-absent-before", async () => {
    const search = await driver.findMarker(marker);
    if (search.found) missing("marker-absent-before", "the marker exists before the device connected");
    return { found: false, characters: search.characters, conversations: search.conversations };
  });
}

async function markerPresent(driver: SyncDriver, step: string, marker: string, context: Record<string, unknown> = {}) {
  let location: { characterId: string; conversationId: string } | undefined;
  await driver.step("sync-result", step, async () => {
    const search = await driver.findMarker(marker);
    if (!search.location)
      missing(step, "the marker is not in the library", { ...context, characters: search.characters, conversations: search.conversations });
    location = search.location;
    return { found: true, markerInLastMessage: search.markerInLastMessage, characters: search.characters, conversations: search.conversations };
  });
  return location!;
}

/** The first device seeds its library, writes `marker`, and publishes to an empty server. */
export async function publishLibrary(driver: SyncDriver, input: { seed: Seed; registration: string; marker: string; characterId: string }) {
  await unboundProfile(driver, input.seed);
  await driver.step("sync-result", "mount", () => driver.mountProduct());
  await markerAbsent(driver, input.marker);
  await driver.step("sync-result", "marker-written", async () => {
    await driver.editLastMessage(input.characterId, input.marker, "sync-publish");
    return { persisted: true };
  });
  await driver.step("sync-result", "settings-open", () => driver.openSyncSettings());
  await driver.step("sync-result", "registration", () => driver.readRegistration(input.registration));
  const connected = await driver.step("sync-result", "connected", () => driver.connect("refuse"));
  await driver.step("sync-result", "published", async () => {
    const pendingAfterConnect = await driver.pendingChanges();
    return { pendingAfterConnect: pendingAfterConnect ?? null, ...(await driver.syncNow("published")) };
  });
  const location = await markerPresent(driver, "marker-kept", input.marker);
  const shown = await driver.step("sync-result", "working-set", () => driver.renderMarker(location, input.marker));
  return { replacementShown: connected.replacementShown, rendered: shown.rendered };
}

/**
 * A device with its own seeded library joins a server library: it confirms the replacement,
 * finds `expect`, writes `marker` into a message and sends it.
 */
export async function receiveAndPush(driver: SyncDriver, input: { seed: Seed; registration: string; expect: string; marker: string }) {
  await unboundProfile(driver, input.seed);
  await driver.step("sync-result", "mount", () => driver.mountProduct());
  await markerAbsent(driver, input.expect);
  const local = await driver.step("sync-result", "local-library", async () => {
    const { hasLocalBindingData } = await import("../../src/ts/storage/sync/bindingLocalData");
    return { nonDefault: await hasLocalBindingData() };
  });
  await driver.step("sync-result", "settings-open", () => driver.openSyncSettings());
  await driver.step("sync-result", "registration", () => driver.readRegistration(input.registration));
  const connected = await driver.step("sync-result", "connected", async () => {
    const outcome = await driver.connect("accept");
    if (outcome.replacementShown && outcome.replacementGatedByCheckbox !== true)
      missing("connected", "the replacement action was not gated by its checkbox", outcome);
    return { ...outcome, localLibrary: local.nonDefault };
  });
  // Without a replacement the product found the server library empty; the marker search tells that apart.
  const location = await markerPresent(driver, "expect-received", input.expect, { replacementShown: connected.replacementShown });
  await driver.step("sync-result", "replacement-asked", async () => {
    if (local.nonDefault && !connected.replacementShown)
      missing("replacement-asked", "the server library replaced local data without the replacement confirmation");
    return { asked: connected.replacementShown };
  });
  const shown = await driver.step("sync-result", "working-set", () => driver.renderMarker(location, input.expect));
  await driver.step("sync-result", "marker-written", async () => {
    await driver.editLastMessage(location.characterId, input.marker, "sync-receive");
    return { persisted: true };
  });
  const pushed = await driver.step("sync-result", "pushed", () => driver.syncNow("pushed"));
  return { replacementShown: connected.replacementShown, rendered: shown.rendered, pushStages: pushed.stages };
}

/** A bound device starts without a registration, reconnects from its stored credential, pulls and finds `expect`. */
export async function reconnectAndPull(driver: SyncDriver, input: { expect: string }) {
  await driver.step("sync-env", "profile", async () => {
    const binding = await driver.bindingTarget();
    if (binding !== "server") throw new SyncStepError("sync-env", "profile", "profile is not bound to the sync server", { binding });
    return { binding };
  });
  await driver.step("sync-result", "mount", () => driver.mountProduct());
  await driver.step("sync-result", "settings-open", () => driver.openSyncSettings());
  await driver.step("sync-result", "reconnected", async () => {
    const outcome = await driver.connectedWithoutRegistration();
    if (outcome.registrationShown) missing("reconnected", "the connected device still asks for a registration");
    return { ...outcome, foundBeforeSync: (await driver.findMarker(input.expect)).found };
  });
  const pulled = await driver.step("sync-result", "pulled", () => driver.syncNow("pulled"));
  const location = await markerPresent(driver, "expect-received", input.expect);
  const shown = await driver.step("sync-result", "working-set", () => driver.renderMarker(location, input.expect));
  return { pullStages: pulled.stages, rendered: shown.rendered };
}
