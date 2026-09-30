import assert from 'node:assert/strict';
import { mkdir, writeFile } from 'node:fs/promises';
import path from 'node:path';

export async function measureLegacyRestore({ args, client, run, pid, packageName, output, apkSha256, environment }) {
  const megabytes = Number(args['legacy-mb']);
  const encoding = args['legacy-encoding'];
  assert.ok([100, 300, 600].includes(megabytes) && ['raw', 'gzip'].includes(encoding), 'Explicit legacy MB and encoding required');
  assert.ok(!args.peer, 'Legacy memory cases require a fresh synthetic process');
  const records = [];
  let latest = { phase: 'preparing', decodedBytes: megabytes * 1_000_000, encoding };
  let peakRssBytes = null;
  let samples = 0;
  let processExited = false;
  let memorySource = null;
  const report = () => ({ schema: 'risunest.synthetic-legacy-restore/v1', platform: 'android-emulator',
    synthetic: true, apkSha256, environment, ...latest, peakRssBytes, memorySamples: samples, memorySource,
    aboveTwiceDecoded: peakRssBytes === null ? null : peakRssBytes > 2 * megabytes * 1_000_000,
    memoryScope: 'native-process-lifetime-high-water-excludes-separate-webcontent',
    peakToDecodedRatio: peakRssBytes === null ? null : peakRssBytes / (megabytes * 1_000_000),
    peakToSourceRatio: peakRssBytes === null || !latest.sourceBytes ? null : peakRssBytes / latest.sourceBytes,
    processExited, processKillConfirmed: null, events: records });
  await mkdir(path.dirname(output), { recursive: true });
  await writeFile(output, JSON.stringify(report(), null, 2));
  await client.evaluate(`void globalThis.__legacyRestoreMeasurement.start(${megabytes}, ${JSON.stringify(encoding)})`, false);
  const deadline = Date.now() + 25 * 60_000;
  let lastPhase;
  while (Date.now() < deadline) {
    const currentPid = (await run(['shell', 'pidof', packageName], 5000, true)).stdout.trim();
    if (currentPid !== pid) { processExited = true; break; }
    const next = await client.evaluate('globalThis.__legacyRestoreMeasurement.state()').catch(() => null);
    if (next) latest = next;
    if (latest.phase !== lastPhase) {
      records.push({ ...latest, hostObservedAt: Date.now() });
      lastPhase = latest.phase;
      await writeFile(output, JSON.stringify(report(), null, 2));
    }
    if (latest.phase === 'restore-started' || latest.phase === 'restore-terminal') {
      const status = await run(['shell', 'run-as', packageName, 'cat', `/proc/${pid}/status`], 5000, true);
      const hwm = /^VmHWM:\s*(\d+)\s+kB$/m.exec(status.stdout);
      if (hwm) {
        peakRssBytes = Math.max(peakRssBytes ?? 0, Number(hwm[1]) * 1024);
        samples++;
        memorySource = 'linux-proc-VmHWM-process-lifetime-kib-converted-to-bytes';
      }
    }
    if (['verified', 'failed'].includes(latest.phase) || latest.phase === 'restore-terminal' && latest.outcome !== 'succeeded') break;
    await new Promise(resolve => setTimeout(resolve, 250));
  }
  const result = report();
  result.success = latest.phase === 'verified' && samples > 0;
  if (!result.success && !processExited && !['failed', 'restore-terminal'].includes(latest.phase)) result.incomplete = true;
  await writeFile(output, JSON.stringify(result, null, 2));
  return result;
}
