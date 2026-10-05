import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { createMacosExitHandler, type MacosExitRequest } from "../../src/ts/storage/macosLifecycle";
import { SaveCoordinator } from "../../src/ts/storage/saveCoordinator";
import { SqlitePersistentDataStore } from "../../src/ts/storage/sqlitePersistentDataStore";
import { createSyncExitCoordinator, type SyncExitDrainResult } from "../../src/ts/storage/syncExitCoordinator";
import { check, pause } from "./contracts";

const report = (stage: string, result: unknown) => invoke("macos_bench_report", { stage, result });

export async function terminationDispatch(phase: string) {
  const scenario = phase.slice("session-dispatch-".length);
  const store = new SqlitePersistentDataStore();
  await store.open();
  const initial = await store.readRoot();
  let root = initial.value;
  let commits = 0;
  let activeCommits = 0;
  let maximumActiveCommits = 0;
  let checkpoints = 0;
  let flushCalls = 0;
  let coalesced = false;
  let remoteStarts = 0;
  let remoteCancels = 0;
  let rendererRequests = 0;
  let sessionRequests = 0;
  let rendererReplies = 0;
  const started = performance.now();
  const timeline: { stage: string; elapsedMillis: number }[] = [];
  const note = (stage: string) => {
    timeline.push({ stage, elapsedMillis: Math.round(performance.now() - started) });
  };
  let releaseCommit!: () => void;
  const commitGate = new Promise<void>(resolve => { releaseCommit = resolve; });
  const commit = store.commit.bind(store);
  store.commit = async input => {
    commits++;
    activeCommits++;
    maximumActiveCommits = Math.max(maximumActiveCommits, activeCommits);
    note("commit-enter");
    try {
      if (scenario === "local") await commitGate;
      return await commit(input);
    } finally {
      activeCommits--;
      note("commit-settled");
    }
  };
  const save = new SaveCoordinator({
    store,
    captureRoot: () => root,
    captureSelectedCharacter: () => null,
    captureCharacter: () => null,
    replaceDatabase: () => { throw new Error("Unexpected database replacement"); },
  });
  save.initialize(initial.revision);
  root = { ...root, username: `synthetic-session-dispatch-${scenario}` };
  save.markPersistentDataDirty(128);
  let firstFlush: Promise<void> | undefined;
  const flushLocal = () => {
    flushCalls++;
    note("flush-enter");
    const pending = save.flushPendingDataLocally("session-dispatch");
    if (firstFlush) coalesced ||= pending === firstFlush;
    else firstFlush = pending;
    return pending;
  };
  const checkpointLocal = async () => {
    checkpoints++;
    note("checkpoint-enter");
    await invoke("pds_checkpoint", { mode: "truncate" });
    note("checkpoint-settled");
  };
  let settleDrain!: (value: SyncExitDrainResult) => void;
  const coordinator = createSyncExitCoordinator({
    flushLocal,
    checkpointLocal,
    acquireEditFence: async () => ({ release: () => note("fence-released") }),
    captureTarget: async () => ({
      revision: save.revision, libraryEpoch: "synthetic-library",
      selectionEpoch: "synthetic-selection", selectionId: "synthetic-remote",
    }),
    selectedDrain: () => scenario === "initial" ? null : ({
      id: "synthetic-remote",
      drain: async (_target, signal) => {
        remoteStarts++;
        note("remote-start");
        signal.addEventListener("abort", () => note("remote-aborted"), { once: true });
        return new Promise<SyncExitDrainResult>(resolve => { settleDrain = resolve; });
      },
      cancel: async () => { remoteCancels++; note("remote-cancelled"); settleDrain?.({ kind: "complete" }); },
    }),
    softWaitMillis: scenario === "dialog" ? 50 : 30_000,
  });
  coordinator.subscribe(state => note(`coordinator-${state.phase}`));
  const snapshot = () => ({
    scenario, route: "self-targeted-apple-event", coordinator: coordinator.snapshot().phase,
    commits, activeCommits, maximumActiveCommits, checkpoints, flushCalls, coalesced,
    remoteStarts, remoteCancels, rendererRequests, sessionRequests, rendererReplies, timeline,
  });
  const handler = createMacosExitHandler({
    coordinator,
    saveLocally: async () => { await flushLocal(); await checkpointLocal(); },
    respond: async (token, exit) => {
      rendererReplies++;
      note(`renderer-response-${exit}`);
      await report("session-dispatch-renderer-response", { ...snapshot(), exit });
      await invoke("macos_exit_response", { token, exit });
    },
    reportError: error => { void report("failure", { stage: "renderer", message: String(error) }); },
  });
  await listen<MacosExitRequest>("risu-macos-exit-requested", ({ payload }) => {
    rendererRequests++;
    if (payload.sessionEnd) sessionRequests++;
    note(payload.sessionEnd ? "renderer-session-request" : "renderer-normal-request");
    void report("session-dispatch-renderer-request", {
      ...snapshot(), sessionEnd: payload.sessionEnd, hasDeadline: payload.deadlineUnixMillis !== undefined,
    }).then(() => handler(payload)).catch(error => report("failure", { message: String(error) }));
  });
  await invoke("macos_bench_main_thread_settled");
  await invoke("macos_lifecycle_ready");
  if (scenario !== "initial") {
    await invoke("macos_bench_native_quit");
    const expected = scenario === "local" ? "saving" : scenario === "drain" ? "syncing" : "remote-delayed";
    const deadline = performance.now() + 15_000;
    while (coordinator.snapshot().phase !== expected || (scenario === "local" && activeCommits !== 1)) {
      check(performance.now() < deadline, "native ordinary quit did not reach the requested coordinator stage");
      await pause(10);
    }
  }
  note("send-session-event");
  await report("session-dispatch-ready", { ...snapshot(), passed: true });
  await invoke("macos_bench_dispatch_session_event");
  await pause(250);
  note("release-local-save");
  releaseCommit();
  await pause(250);
  await report("session-dispatch-after-delivery", snapshot());
  // A delivered upgrade may leave the original coordinator awaiting a decision.
  if (scenario !== "initial") coordinator.decide("cancel-exit");
  await pause(6_000);
  await report("failure", { ...snapshot(), message: "Session event did not terminate the isolated app" });
  await invoke("macos_bench_quit");
}
