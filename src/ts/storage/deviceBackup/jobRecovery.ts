import { language } from "../../../lang";
import { portableBackupSuggestedName } from "../portableBackupName";
import { invoke } from "@tauri-apps/api/core";
import { isTauriIOS } from "../../platform";
import { alertNormal, alertSelect } from "../../alert";
import {
  createPortableExportIntentStore,
  portableAndroidPublicationDependencies,
  PortableExportNeedsAttention,
  androidReceiptSettled,
  rememberPortableExport,
  resumePendingPortableExport,
  type PendingPortableExport,
  type PortableJobStatus,
} from "./job";

let running: Promise<void> | undefined;

async function recover(): Promise<void> {
  if (
    !(window as Window & { __TAURI_INTERNALS__?: unknown }).__TAURI_INTERNALS__
  )
    return;
  const text = language.portableBackup.recovery;
  const store = createPortableExportIntentStore();
  const jobs = await invoke<PortableJobStatus[]>("native_file_job_list");
  const portable = jobs.filter((job) => job.kind === "export-portable-backup");
  const stored = store.read();
  if (stored && !portable.some((job) => job.jobId === stored.jobId)) {
    if (stored.phase === "published") store.clear(stored.jobId);
    else {
      const choice = await alertSelect(
        [text.dismiss, text.later],
        text.unavailable,
      );
      if (choice !== "0") return;
      store.clear(stored.jobId);
    }
  }
  for (const job of portable) {
    const existing = store.read();
    if (existing && existing.jobId !== job.jobId) continue;
    if (!existing) {
      const choice = await alertSelect(
        [text.review, text.later],
        text.needsReview,
      );
      if (choice !== "0") continue;
      let status = await invoke<PortableJobStatus>("native_file_job_status", {
        jobId: job.jobId,
      });
      let deadline = Date.now() + 60_000;
      while (!["succeeded", "failed", "cancelled"].includes(status.state)) {
        if (Date.now() >= deadline) {
          const choice = await alertSelect([text.wait, text.later], text.stillRunning);
          if (choice !== "0") return;
          deadline = Date.now() + 60_000;
        }
        await new Promise((resolve) => setTimeout(resolve, 100));
        status = await invoke<PortableJobStatus>("native_file_job_status", {
          jobId: job.jobId,
        });
      }
      rememberPortableExport(
        job.jobId,
        status.result?.handoffPath
          ? {
              type: isTauriIOS ? "iosFiles" : "androidSaf",
              suggestedName: portableBackupSuggestedName(),
            }
          : { type: "desktopPath" },
        store,
      );
    }
    while (store.read()?.jobId === job.jobId) {
      try {
        await resumePendingPortableExport({
          invoke,
          store,
          wait: (milliseconds) =>
            new Promise((resolve) => setTimeout(resolve, milliseconds)),
          ...portableAndroidPublicationDependencies(),
          cleanupHandoff: async (path) => {
            await invoke("native_portable_handoff_cleanup", { path });
          },
          onResult: (result) =>
            alertNormal(
              result.warningCodes.length
                ? text.savedWarnings
                : text.saved,
            ),
        });
      } catch (error) {
        const intent = store.read();
        if (!intent) throw error;
        const status = await invoke<PortableJobStatus>(
          "native_file_job_status",
          { jobId: job.jobId },
        );
        const published =
          error instanceof PortableExportNeedsAttention &&
          !!error.committedResult;
        const partial =
          error instanceof PortableExportNeedsAttention &&
          error.warningCodes.includes("partial-destination-may-remain");
        const canRetry = status.state === "succeeded";
        const labels = canRetry
          ? [
              published ? text.retryCleanup : text.retrySave,
              text.discard,
              text.later,
            ]
          : [text.dismiss, text.later];
        const code =
          error instanceof PortableExportNeedsAttention
            ? error.code
            : "portable-export-recovery-failed";
        const choice = await alertSelect(
          labels,
          `${published ? text.cleanupNeeded : text.needsAttention} (${code})${partial ? text.partialFile : ""}`,
        );
        if (choice === "0" && canRetry) {
          if (!published) {
            await ensureAndroidReceiptSettled(intent);
            // User explicitly chose another save attempt. The native archive is
            // retained and no old destination URI is replayed from backup data.
            store.write({
              ...intent,
              phase: "waiting-native",
              requestId: undefined,
            });
          }
          continue;
        }
        if ((choice === "1" && canRetry) || (choice === "0" && !canRetry)) {
          await discard(intent, status);
          store.clear(intent.jobId);
          break;
        }
        return;
      }
    }
  }
}

async function discard(
  intent: PendingPortableExport,
  status: PortableJobStatus,
): Promise<void> {
  await ensureAndroidReceiptSettled(intent);
  if (status.result?.handoffPath)
    await invoke("native_portable_handoff_cleanup", {
      path: status.result.handoffPath,
    });
  await invoke("native_file_job_forget", { jobId: intent.jobId });
}

async function ensureAndroidReceiptSettled(intent: PendingPortableExport): Promise<void> {
  if (intent.publication !== "android-saf" || !intent.requestId) return;
  const publication = portableAndroidPublicationDependencies();
  if (
    !await androidReceiptSettled(publication, intent.requestId)
  )
    throw new PortableExportNeedsAttention(
      "android-publication-still-active",
      intent.jobId,
    );
}

/** Runs only after normal bootstrap; concurrent callers share one recovery flow. */
export function resumePortableExportsAfterBootstrap(): Promise<void> {
  if (!running)
    running = recover()
      .catch(async () => {
        const text = language.portableBackup.recovery;
        await alertSelect(
          [text.later],
          text.recoveryFailed,
        );
      })
      .finally(() => {
        running = undefined;
      });
  return running;
}
