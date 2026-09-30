import { invoke, type InvokeArgs } from "@tauri-apps/api/core";
import syntheticDatabase from "../../src-tauri/fixtures/persistent-fixture.json";

const iterations = 1000;
const owner = "synthetic-lease-release-owner";

type Display = {
  configure(mode: "recent", defer: boolean): Promise<void>;
  publish(source: string, active?: boolean): Promise<void>;
};

export function leaseReleaseStatistics(values: number[]) {
  if (!values.length) return null;
  const ordered = [...values].sort((a, b) => a - b);
  const rank = (percent: number) => ordered[Math.max(0, Math.ceil(percent * ordered.length / 100) - 1)];
  return { samples: ordered.length, min: ordered[0], p50: rank(50), p95: rank(95), max: ordered.at(-1)! };
}

export async function runLeaseReleaseCheckpointSuite(display: Display) {
  let phase = "setup";
  const opened = await invoke<{ revision: number }>("pds_open");
  const stage = await invoke<{ stagingId: string }>("pds_replace_begin");
  const { characters, botPresets, ...root } = syntheticDatabase;
  await invoke("pds_replace_put_root", { stagingId: stage.stagingId, root });
  await invoke("pds_replace_put_presets", { stagingId: stage.stagingId, presets: botPresets });
  await invoke("pds_replace_add_characters", { stagingId: stage.stagingId, characters });
  let { revision } = await invoke<{ revision: number }>("pds_replace_commit", {
    stagingId: stage.stagingId, expectedRevision: opened.revision,
  });
  await display.configure("recent", true);
  const cases: Record<string, unknown>[] = [];
  for (const scenario of ["read-release-no-new-commit", "commit-read-release", "registered-reader-growing-wal"]) {
    phase = scenario;
    let anchor: string | undefined;
    let currentLease: string | undefined;
    const acquireMs: number[] = [], readMs: number[] = [], releaseMs: number[] = [], commitMs: number[] = [];
    const frameGaps: number[] = [];
    const longTasks: number[] = [];
    const supported = PerformanceObserver.supportedEntryTypes.includes("longtask");
    const observer = supported ? new PerformanceObserver(list => longTasks.push(...list.getEntries().map(entry => entry.duration))) : undefined;
    let active = true;
    let lastFrame = performance.now();
    let frameId = 0;
    const frame = (now: number) => {
      frameGaps.push(now - lastFrame); lastFrame = now;
      if (active) frameId = requestAnimationFrame(frame);
    };
    let publications = 0;
    let pending = Promise.resolve();
    let publishing = false;
    let displayFailed = false;
    const publish = () => {
      if (publishing) return;
      publishing = true;
      publications++;
      pending = display.publish(`<Thoughts>Synthetic ${publications}</Thoughts>\nAnswer ${publications}`)
        .catch(() => { displayFailed = true; })
        .finally(() => { publishing = false; });
    };
    const measured = async <T>(command: string, args: InvokeArgs, samples: number[]) => {
      const start = performance.now();
      const result = await invoke<T>(command, args);
      samples.push(performance.now() - start);
      return result;
    };
    observer?.observe({ entryTypes: ["longtask"] });
    frameId = requestAnimationFrame(frame);
    publish();
    const timer = setInterval(publish, 33);
    try {
      if (scenario === "registered-reader-growing-wal") {
        anchor = (await invoke<{ lease: string }>("pds_acquire_revision", { revision })).lease;
      }
      for (let index = 0; index < iterations; index++) {
        if (index % 100 === 0) (window as any).__streamingSmokeProgress?.(`persistence-lease-release-${scenario}-${index}`);
        currentLease = (await measured<{ lease: string }>("pds_acquire_revision", { revision }, acquireMs)).lease;
        if (scenario !== "read-release-no-new-commit") {
          const saved = await measured<{ revision: number }>("pds_commit", {
            commit: { expectedRevision: revision, pluginStorage: [{ type: "set", owner, key: "value", value: `${index}:${"s".repeat(256)}` }] },
            assetAliases: [],
          }, commitMs);
          if (saved.revision !== revision + 1) throw new Error("synthetic-commit-revision");
          revision = saved.revision;
        }
        await measured("pds_read_root", { lease: currentLease }, readMs);
        await measured("pds_release_revision", { lease: currentLease }, releaseMs);
        currentLease = undefined;
      }
      await pending;
      if (displayFailed) throw new Error("synthetic-stream-display");
      cases.push({ scenario, commits: commitMs.length, releases: releaseMs.length,
        acquireIpcMs: leaseReleaseStatistics(acquireMs), readIpcMs: leaseReleaseStatistics(readMs),
        releaseIpcMs: leaseReleaseStatistics(releaseMs), commitIpcMs: leaseReleaseStatistics(commitMs),
        frameGapMs: leaseReleaseStatistics(frameGaps), longTaskMs: supported ? leaseReleaseStatistics(longTasks) : null,
        publications, releaseSamplesMs: releaseMs, checkpointSqlRequests: null, walBytes: null, backfilledCheckpointCount: null });
    } catch {
      return { passed: false, benchmark: "lease-release-ipc", assertion: `lease-release-${phase}`, cases };
    } finally {
      clearInterval(timer); active = false; cancelAnimationFrame(frameId); observer?.disconnect();
      await pending;
      if (currentLease) await invoke("pds_release_revision", { lease: currentLease });
      if (anchor) await invoke("pds_release_revision", { lease: anchor });
      await display.publish("Synthetic streaming complete", false);
    }
  }
  return { passed: true, benchmark: "lease-release-ipc", synthetic: true, iterations, cases,
    latencyEvidence: "Direct production pds_release_revision IPC calls during isolated synthetic display streaming; includes IPC, store-lock wait and native work.",
    measurementGaps: ["Existing IPC exposes no checkpoint counters or WAL size; null fields are unavailable measurements, not zero.",
      "No unregistered native reader can be held by this frontend. Use the isolated native benchmark for that case.",
      "Native backfilled-checkpoint attribution and physical-device performance remain separate measurements."] };
}
