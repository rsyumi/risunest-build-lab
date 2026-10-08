import { SyncDriver, SyncStepError, redact } from "./driver";
import { errorShape } from "./externalStorage";

/** A RisuNest backup file round trip through the product's export and restore routes, without the system file pickers. */
type Seed = () => Promise<unknown>;

const missing = (step: string, message: string, detail: Record<string, unknown> = {}): never => {
  throw new SyncStepError("sync-result", step, message, detail);
};

async function attempt<T>(step: string, run: () => Promise<T>, detail: () => Record<string, unknown> = () => ({})): Promise<T> {
  try {
    return await run();
  } catch (error) {
    if (error instanceof SyncStepError) throw error;
    return missing(step, "the product refused the operation", {
      ...errorShape(error), message: error instanceof Error ? redact(error.message).slice(0, 160) : null, ...detail(),
    });
  }
}

async function withDeadline<T>(ms: number, run: Promise<T>, expired: () => never): Promise<T> {
  let timer: ReturnType<typeof setTimeout> | undefined;
  try {
    return await Promise.race([run, new Promise<never>((_, reject) => { timer = setTimeout(() => { try { expired(); } catch (error) { reject(error); } }, ms); })]);
  } finally {
    clearTimeout(timer);
  }
}

/** Reads and closes the result the file operation dialog holds, as a person would after reading it. */
async function closeOutcome() {
  const { get } = await import("svelte/store");
  const { nativeFileOperationOutcome } = await import("../../src/ts/storage/nativeFileJobManager");
  const outcome = get(nativeFileOperationOutcome);
  nativeFileOperationOutcome.set(null);
  return outcome?.status ? { state: outcome.status.state, phase: outcome.status.phase ?? null } : null;
}

function buttons(root: ParentNode, label: string) {
  return [...root.querySelectorAll<HTMLButtonElement>("button")].filter((button) => {
    const copy = button.cloneNode(true) as HTMLElement;
    copy.querySelectorAll(".sr-only").forEach((node) => node.remove());
    return copy.textContent?.trim() === label;
  });
}

type Dialog = { type: string; msg?: string; checkboxConfirm?: { title: string } };

/** Names and closes the dialog on top, as its close button would; the record names it by type and string key. */
async function closeDialog(driver: SyncDriver) {
  const { get } = await import("svelte/store");
  const { alertStore } = await import("../../src/ts/stores.svelte");
  if (!alertStore.dialogVisible()) return null;
  const described = await driver.describeDialog(get(alertStore) as Dialog);
  alertStore.set({ type: "none", msg: "" });
  return described;
}

/**
 * Ticks and accepts the restore confirmation the product shows before it replaces the library, while
 * `running` lasts. Any other dialog is recorded and closed, which the restore treats as a cancel.
 */
async function answerRestoreConfirmation(driver: SyncDriver, running: Promise<unknown>) {
  const { get } = await import("svelte/store");
  const { alertStore } = await import("../../src/ts/stores.svelte");
  const { language } = await import("../../src/lang");
  const shown = { confirmationShown: false, confirmationGatedByCheckbox: null as boolean | null, otherDialogs: [] as unknown[] };
  let settled = false;
  running.then(() => { settled = true; }, () => { settled = true; });
  const deadline = Date.now() + 120_000;
  while (!settled && !shown.confirmationShown && Date.now() < deadline) {
    if (alertStore.dialogVisible()) {
      const value = get(alertStore) as Dialog;
      const dialog = document.querySelector<HTMLElement>('[role="dialog"][aria-labelledby="checkbox-confirm-title"]');
      const box = dialog?.querySelector<HTMLInputElement>('input[type="checkbox"]');
      const action = dialog ? buttons(dialog, language.lwwSync.restoreAction) : [];
      if (value.type !== "checkboxConfirm" || value.checkboxConfirm?.title !== language.lwwSync.restoreTitle) {
        shown.otherDialogs.push(await closeDialog(driver));
      } else if (dialog && box && action.length === 1) {
        shown.confirmationShown = true;
        shown.confirmationGatedByCheckbox = action[0].disabled && !box.checked;
        box.click();
        await new Promise((resolve) => setTimeout(resolve, 50));
        if (!box.checked || action[0].disabled) {
          shown.otherDialogs.push({ restoreActionDisabledAfterAcknowledging: true });
          await closeDialog(driver);
        } else action[0].click();
      }
    }
    await new Promise((resolve) => setTimeout(resolve, 100));
  }
  return shown;
}

async function markers(driver: SyncDriver, step: string, present: string, absent: string) {
  const found = await driver.findMarker(present);
  if (!found.location) missing(step, "the expected message is not in the library", { characters: found.characters });
  if ((await driver.findMarker(absent)).found) missing(step, "the replaced message is still in the library");
  return found.location!;
}

/**
 * Seeds an unbound profile, writes `before` into the last message, exports a RisuNest backup to a
 * temporary file, replaces the message with `after`, restores the file on the same app and checks that
 * the library and the open chat hold `before` again.
 */
export async function backupFileRestore(driver: SyncDriver, input: { seed: Seed; characterId: string; before: string; after: string }) {
  await driver.step("sync-env", "profile", async () => {
    const binding = await driver.bindingTarget();
    if (binding !== "none") throw new SyncStepError("sync-env", "profile", "profile is already bound to a sync target", { binding });
    await driver.prepareProfile(input.seed);
    return { binding, seeded: true };
  });
  await driver.step("sync-result", "mount", () => driver.mountProduct());
  await driver.step("sync-result", "marker-written", async () => {
    await driver.editLastMessage(input.characterId, input.before, "backup-file-before");
    return { persisted: true };
  });
  const { invoke } = await import("@tauri-apps/api/core");
  const { join, tempDir } = await import("@tauri-apps/api/path");
  const path = await join(await tempDir(), `risunest-harness-${crypto.randomUUID()}.risunest`);
  const exported = await driver.step("sync-result", "exported", async () => {
    const { runSharedNativeFileOperation } = await import("../../src/ts/storage/nativeFileJobManager");
    const { runNativeArchiveExport } = await import("../../src/ts/storage/nativeFileJobs");
    const { selectPortableBackupExport } = await import("../../src/ts/storage/deviceBackup/selectionDialog");
    const { getPersistentDataRuntime } = await import("../../src/ts/storage/persistentDataRuntime.svelte");
    // The same route as the export button, with the save dialog's answer given directly.
    const result = await attempt("exported", () => runSharedNativeFileOperation("export", "portable-backup-export",
      async ({ signal, onStatus }) => runNativeArchiveExport(getPersistentDataRuntime(), { type: "desktopPath", path },
        await selectPortableBackupExport(), { signal, onStatus, confirmSourcePreservation: async () => false }),
      { presentation: "dialog", format: "library-backup" }));
    const format = await attempt("exported", () => invoke<string>("native_backup_source_format", { source: { type: "desktopPath", path } }));
    if (format !== "portable") missing("exported", "the exported file is not a RisuNest backup", { format });
    return { format, sourceBytes: result.sourceBytes, characterCount: result.characterCount, warningCodes: result.warningCodes, outcome: await closeOutcome() };
  });
  await driver.step("sync-result", "changed", async () => {
    await driver.editLastMessage(input.characterId, input.after, "backup-file-after");
    await markers(driver, "changed", input.after, input.before);
    return { persisted: true };
  });
  const restored = await driver.step("sync-result", "restored", async () => {
    const { restoreBackupFromNativeSource } = await import("../../src/ts/storage/portableBackupFileRouteProduction.svelte");
    const last = { state: "", phase: "" as string | null, changes: 0 };
    const detail = () => ({ lastState: last.state, lastPhase: last.phase, statusChanges: last.changes });
    // The same route as the restore button, with the file picker's answer given directly.
    const running = attempt("restored", () => restoreBackupFromNativeSource({ type: "desktopPath", path }, {
      onStatus(status) {
        if (status.state !== last.state || (status.phase ?? null) !== last.phase) last.changes++;
        last.state = status.state;
        last.phase = status.phase ?? null;
      },
    }), detail);
    const confirmation = await answerRestoreConfirmation(driver, running);
    const result = await withDeadline(300_000, running, () => missing("restored", "the restore did not finish", { ...detail(), ...confirmation }));
    if (!result) missing("restored", "the restore was cancelled", { ...detail(), ...confirmation });
    // A preservation report opens a notice the restore does not wait for.
    await new Promise((resolve) => setTimeout(resolve, 500));
    const notice = await closeDialog(driver);
    return { warningCodes: result!.warningCodes, ...detail(), ...confirmation, notice, outcome: await closeOutcome() };
  });
  const location = await driver.step("sync-result", "library-restored", async () => {
    const found = await markers(driver, "library-restored", input.before, input.after);
    return { characterId: found.characterId, conversationId: found.conversationId };
  });
  const shown = await driver.step("sync-result", "working-set", () => driver.renderMarker(location, input.before));
  return { backupBytes: exported.sourceBytes, restoreWarnings: restored.warningCodes, rendered: shown.rendered };
}

/** After a relaunch the product opens on the restored library, which still holds `before` and not `after`. */
export async function backupFileAfterRelaunch(driver: SyncDriver, input: { before: string; after: string }) {
  await driver.step("sync-env", "profile", async () => ({ binding: await driver.bindingTarget() }));
  await driver.step("sync-result", "mount", () => driver.mountProduct());
  const location = await driver.step("sync-result", "library-kept", async () => {
    const found = await markers(driver, "library-kept", input.before, input.after);
    return { characterId: found.characterId, conversationId: found.conversationId };
  });
  const shown = await driver.step("sync-result", "working-set", () => driver.renderMarker(location, input.before));
  return { rendered: shown.rendered };
}
