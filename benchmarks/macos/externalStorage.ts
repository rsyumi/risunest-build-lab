import { invoke } from "@tauri-apps/api/core";
import { SyncDriver, SyncStepError } from "../sync-ui/driver";
import { webDavAfterRelaunch, webDavBackupRestore, type WebDavInput } from "../sync-ui/externalStorage";
import { initialize } from "./contracts";

interface ExternalInput {
  webdav?: WebDavInput;
  before?: string;
  after?: string;
}

const report = (stage: string, result: unknown) => invoke("macos_bench_report", { stage, result });

/** One launch of the WebDAV backup round trip; the controller serves WebDAV and hands each phase its inputs. */
export async function externalStoragePhase(phase: string) {
  let input: ExternalInput | null = null;
  try {
    input = JSON.parse((await invoke<string | null>("macos_bench_expected")) ?? "null");
  } catch {}
  if (!input?.before) throw new SyncStepError("sync-env", "input", "controller did not hand over external storage inputs");
  const driver = new SyncDriver(phase, report);
  const result = phase === "external-storage"
    ? await (() => {
      if (!input.webdav || !input.after) throw new SyncStepError("sync-env", "input", "controller did not hand over the WebDAV server");
      return webDavBackupRestore(driver, {
        seed: initialize, platform: "macos", characterId: "char-a", webdav: input.webdav, before: input.before, after: input.after,
      });
    })()
    : phase === "external-storage-restart"
      ? await webDavAfterRelaunch(driver, { before: input.before })
      : undefined;
  if (!result) throw new SyncStepError("sync-env", "input", "unknown external storage phase");
  await report(phase, { passed: true, label: "sync-result", ...result });
  await driver.quitWatchingExit(() => invoke("macos_bench_quit"));
}
