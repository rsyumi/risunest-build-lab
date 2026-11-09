import "../../src/ts/androidNativeControl";
import { invoke } from "@tauri-apps/api/core";
import { alertStore } from "../../src/ts/stores.svelte";
import { registerAndroidRisuSaveRoute } from "../../src/ts/storage/androidRisuSaveRouteProduction.svelte";
import { resumePortableExportsAfterBootstrap } from "../../src/ts/storage/deviceBackup/jobRecovery";
import { createPortableExportIntentStore } from "../../src/ts/storage/deviceBackup/job";
import { runNativeArchiveExport } from "../../src/ts/storage/nativeFileJobs";
import { nativePortableDeviceSections } from "../../src/ts/storage/deviceBackup/selection";
import { getAndroidSafExportStatus, type AndroidSafDestinationEvent } from "../../src/ts/storage/androidSafBridge";
import { writeNativePluginDeviceValues } from "../../src/ts/plugins/pluginDeviceTransfer";
import { sendAndroidCommit } from "../../src/ts/storage/androidCommitTransport";
import { beginCasJob, prepareCasObject, releaseCasJob } from "../../src/ts/storage/nativeAssetRepository";
import type { PersistentDataRuntime } from "../../src/ts/storage/persistentDataRuntime";

const key = "risunest-synthetic-android-publication";
const owner = "android-publication-probe";

type ProbeState = {
  phase: string;
  requestId?: string;
  suggestedName?: string;
  copyBytes?: number;
  sourceBytes?: number;
  terminal?: AndroidSafDestinationEvent;
  dialogs: { type: string; message: string }[];
  error?: string;
};

export function androidPublicationProbe(dependencies: {
  ready(): Promise<void>;
  runtime(): Promise<PersistentDataRuntime>;
}) {
  let state: ProbeState = JSON.parse(localStorage.getItem(key) ?? 'null') ?? {phase: "idle", dialogs: []};
  let pauseAt: "copy" | "terminal" | undefined;
  const save = () => localStorage.setItem(key, JSON.stringify(state));
  const fail = (error: unknown) => {
    state.error = error instanceof Error ? error.message : String(error);
    state.phase = "failed";
    save();
  };
  alertStore.subscribe((dialog) => {
    if (["normal", "error", "select"].includes(dialog.type)) {
      state.dialogs.push({type: dialog.type, message: dialog.msg});
      save();
    }
  });
  window.addEventListener("risu-android-saf-progress", (event) => {
    const detail = (event as CustomEvent<{operation: string; copiedBytes: number}>).detail;
    if (detail?.operation !== "destination-copy") return;
    state.copyBytes = detail.copiedBytes;
    state.phase = "copying";
    save();
    if (pauseAt === "copy" && detail.copiedBytes > 0) {
      pauseAt = undefined;
      debugger;
    }
  }, true);
  window.addEventListener("risu-android-saf-destination", (event) => {
    const terminal = (event as CustomEvent<AndroidSafDestinationEvent>).detail;
    state.terminal = terminal;
    save();
    if (pauseAt === "terminal" && terminal.state === "succeeded") {
      pauseAt = undefined;
      debugger;
    }
  }, true);

  const bootstrap = dependencies.ready().then(async () => {
    if (["exporting", "copying"].includes(state.phase)) {
      await dependencies.runtime();
      registerAndroidRisuSaveRoute();
      state.phase = "recovering";
      state.dialogs = [];
      save();
      await resumePortableExportsAfterBootstrap();
      state.phase = "recovered";
      save();
    }
  }).catch(fail);

  let transport: Record<string, unknown> = {phase: "idle"};
  return {
    state: () => ({...state}),
    async receipt() { return {status: await getAndroidSafExportStatus(), intent: createPortableExportIntentStore().read()}; },
    closeDialog(choice = "1") { alertStore.set({type: "none", msg: choice}); },
    async start(mebibytes = 32, stopAt?: "copy" | "terminal") {
      await bootstrap;
      if (!Number.isInteger(mebibytes) || mebibytes < 1 || mebibytes > 128) throw new Error("Invalid synthetic size");
      if (createPortableExportIntentStore().read()) throw new Error("Previous synthetic publication remains pending");
      await dependencies.runtime();
      registerAndroidRisuSaveRoute();
      state = {phase: "seeding", dialogs: []}; save();
      let value = "";
      let random = 0x12345678;
      const chars: string[] = [];
      for (let index = 0; index < mebibytes * 1024; index++) {
        chars.length = 0;
        for (let offset = 0; offset < 1024; offset++) {
          random ^= random << 13; random ^= random >>> 17; random ^= random << 5;
          chars.push(String.fromCharCode(33 + ((random >>> 0) % 90)));
        }
        value += chars.join("");
      }
      await writeNativePluginDeviceValues(owner, [{type: "set", space: "string", key: "payload", value}]);
      state.phase = "exporting"; state.suggestedName = `synthetic-publication-${Date.now()}.risunest`; pauseAt = stopAt; save();
      try {
        const result = await runNativeArchiveExport(await dependencies.runtime(),
          {type: "androidSaf", suggestedName: state.suggestedName},
          {library: true, deviceSections: [...nativePortableDeviceSections]}, {
            onStatus(status) {
              if (status.result) state.sourceBytes = status.result.sourceBytes;
              state.requestId = createPortableExportIntentStore().read()?.requestId;
              save();
            },
          });
        state.phase = "completed";
        state.sourceBytes = result.sourceBytes;
        save();
      } catch (error) { fail(error); }
    },
    transportState: () => ({...transport, pending: transport.pending instanceof Set ? [...transport.pending] : []}),
    async transport(count = 1024, keyBytes = 512, valueBytes = 1024) {
      await bootstrap;
      if (!Number.isInteger(count) || count < 1 || count > 1024 || !Number.isInteger(keyBytes) || keyBytes < 8 || keyBytes > 512
        || !Number.isInteger(valueBytes) || valueBytes < 0 || valueBytes > 32768) throw new Error("Invalid synthetic batch dimensions");
      const pending = new Set<string>();
      const calls: {command: string; started: number; duration?: number}[] = [];
      const observed = async <T>(command: string, args?: Record<string, unknown>): Promise<T> => {
        const call = {command, started: performance.now(), duration: undefined as number | undefined};
        calls.push(call); pending.add(command);
        try { return await invoke<T>(command, args); }
        finally { call.duration = performance.now() - call.started; pending.delete(command); }
      };
      const mutations = Array.from({length: count}, (_, index) => ({type: "set", space: "string",
        key: String(index).padStart(8, "0") + "k".repeat(keyBytes - 8), value: "v".repeat(valueBytes)}));
      const bytes = new TextEncoder().encode(JSON.stringify({owner, mutations, observations: []}));
      transport = {phase: "running", count, keyBytes, valueBytes, requestBytes: bytes.length, calls, pending};
      try {
        const sessionId = await beginCasJob("direct-asset-or-inlay-write");
        try {
          const first = sendAndroidCommit<unknown[]>(bytes, observed, undefined, "pds_plugin_write_android_finish").then(result => {
            transport.replyBytes = new TextEncoder().encode(JSON.stringify(result)).length;
            transport.firstCompleted = true;
          });
          const second = sendAndroidCommit(new TextEncoder().encode(JSON.stringify({owner,
            mutations: [{type: "set", space: "string", key: "later", value: "ok"}], observations: []})), observed, undefined,
            "pds_plugin_write_android_finish").then(() => { transport.secondCompleted = true; });
          const upload = prepareCasObject(sessionId, new Uint8Array(512 * 1024).fill(42), "direct-object")
            .then(result => { transport.uploadBytes = result.byteSize; });
          await Promise.all([first, second, upload]);
          transport.phase = "completed";
        } finally { await releaseCasJob(sessionId, "aborted"); }
      } catch (error) { transport.phase = "failed"; transport.error = error instanceof Error ? error.message : String(error); }
    },
    async nativeSlotAvailable() {
      const id = crypto.randomUUID();
      try { await invoke("pds_commit_android_open", {id, totalBytes: 2, binary: false}); return true; }
      catch (error) { return {error}; }
      finally { await invoke("pds_commit_android_cancel", {id}); }
    },
  };
}
