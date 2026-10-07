import { invoke } from "@tauri-apps/api/core";
import { SyncDriver, SyncStepError, redact } from "../sync-ui/driver";
import { webDavAfterRelaunch, webDavBackupRestore, type WebDavInput } from "../sync-ui/externalStorage";
import { initialize } from "./contracts";

interface ExternalInput {
  webdav?: WebDavInput;
  before: string;
  after?: string;
}

const report = (stage: string, result: unknown) => invoke("ios_bench_report", { stage, result });

/**
 * Backs up to a new WebDAV repository and restores it (`external-storage`), or checks the
 * connection and the restored library after a relaunch (`external-storage-restart`). XCTest reads
 * the outcome from the same fixed overlay the Sync phases use.
 */
export async function externalStorageContract(phase: "external-storage" | "external-storage-restart") {
  const overlay = document.createElement("pre");
  overlay.style.cssText = "position:fixed;left:0;right:0;bottom:0;z-index:2147483647;margin:0;padding:4px 8px;"
    + "font:12px monospace;white-space:pre-wrap;pointer-events:none;background:#000;color:#fff";
  document.body.append(overlay);
  const driver = new SyncDriver(phase, report, (step) => { overlay.textContent = `sync-step:${phase}:${step}`; });
  try {
    let input: ExternalInput;
    try {
      input = await invoke<ExternalInput>("ios_bench_external_input");
    } catch {
      throw new SyncStepError("sync-env", "input", "external storage inputs were not supplied");
    }
    const result = phase === "external-storage"
      ? await webDavBackupRestore(driver, {
        seed: initialize, platform: "ios", characterId: "char-a", webdav: input.webdav!, before: input.before, after: input.after!,
      })
      : await webDavAfterRelaunch(driver, { before: input.before });
    await report(phase, { passed: true, label: "sync-result", ...result });
    overlay.textContent = `sync-result:passed:${JSON.stringify({ phase, ...result })}`;
  } catch (error) {
    const detail = error instanceof SyncStepError ? error.detail : { label: "sync-result", step: "unknown" };
    const message = error instanceof Error ? redact(error.message) : "failed";
    await report("failure", { passed: false, message, detail }).catch(() => {});
    overlay.textContent = `sync-result:failed:${JSON.stringify({ phase, ...detail, message })}`;
  }
}
