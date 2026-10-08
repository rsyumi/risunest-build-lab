import { invoke } from "@tauri-apps/api/core";
import { SyncDriver, SyncStepError } from "../sync-ui/driver";
import { webDavAfterRelaunch, webDavBackupRestore, type WebDavInput } from "../sync-ui/externalStorage";
import { webDavSyncAfterRelaunch, webDavSyncPublish } from "../sync-ui/externalSync";
import { backupFileAfterRelaunch, backupFileRestore } from "../sync-ui/portableBackup";
import { initialize } from "./contracts";

interface ExternalInput {
  webdav?: WebDavInput;
  before?: string;
  after?: string;
}

const report = (stage: string, result: unknown) => invoke("macos_bench_report", { stage, result });

/** One launch of a WebDAV or backup file round trip; the controller hands each phase its inputs. */
export async function externalStoragePhase(phase: string) {
  let input: ExternalInput | null = null;
  try {
    input = JSON.parse((await invoke<string | null>("macos_bench_expected")) ?? "null");
  } catch {}
  if (!input?.before) throw new SyncStepError("sync-env", "input", "controller did not hand over external storage inputs");
  const driver = new SyncDriver(phase, report);
  const webdav = () => {
    if (!input.webdav || !input.after) throw new SyncStepError("sync-env", "input", "controller did not hand over the WebDAV server");
    return input.webdav;
  };
  const after = () => {
    if (!input.after) throw new SyncStepError("sync-env", "input", "controller did not hand over the second marker");
    return input.after;
  };
  const result = phase === "external-storage"
    ? await webDavBackupRestore(driver, {
      seed: initialize, platform: "macos", characterId: "char-a", webdav: webdav(), before: input.before, after: after(),
    })
    : phase === "external-storage-restart"
      ? await webDavAfterRelaunch(driver, { before: input.before })
      : phase === "external-sync"
        ? await webDavSyncPublish(driver, { seed: initialize, platform: "macos", characterId: "char-a", webdav: webdav(), first: input.before })
        : phase === "external-sync-restart"
          ? await webDavSyncAfterRelaunch(driver, { characterId: "char-a", first: input.before, second: after() })
          : phase === "backup-file"
            ? await backupFileRestore(driver, { seed: initialize, characterId: "char-a", before: input.before, after: after() })
            : phase === "backup-file-restart"
              ? await backupFileAfterRelaunch(driver, { before: input.before, after: after() })
              : undefined;
  if (!result) throw new SyncStepError("sync-env", "input", "unknown external storage phase");
  await report(phase, { passed: true, label: "sync-result", ...result });
  await driver.quitWatchingExit(() => invoke("macos_bench_quit"));
}
