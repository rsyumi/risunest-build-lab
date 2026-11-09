import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { open, writeFile } from 'node:fs/promises';
import path from 'node:path';
import { pack } from 'msgpackr';

const nameLength = 384;
const bodyLength = 32;
const pause = milliseconds => new Promise(resolve => setTimeout(resolve, milliseconds));
const digest = value => createHash('sha256').update(value).digest('hex');
const name = index => `entry-${String(index).padStart(8, '0')}-`.padEnd(nameLength, 'x');
const body = index => createHash('sha256').update(`synthetic-entry-${index}`).digest();
function frame(name, bytes) {
  const encoded = Buffer.from(name);
  const result = Buffer.alloc(8 + encoded.length + bytes.length);
  result.writeUInt32LE(encoded.length);
  encoded.copy(result, 4);
  result.writeUInt32LE(bytes.length, 4 + encoded.length);
  bytes.copy(result, 8 + encoded.length);
  return result;
}

export async function measureEntryCount({ count, directory, evaluate, memory, save, prepareSource = async file => file }) {
  assert.ok([20_000, 50_000, 100_000, 200_001].includes(count), 'Unknown entry-count case');
  const invoke = (command, args = {}) => evaluate(`globalThis.__TAURI_INTERNALS__.invoke(${JSON.stringify(command)},${JSON.stringify(args)})`);
  const readyDeadline = Date.now() + 120_000;
  while (Date.now() < readyDeadline) {
    const state = await evaluate('globalThis.__deviceBackupSmoke?.state() ?? null');
    if (!state) { await pause(100); continue; }
    assert.ok(!state.failure, state.failure);
    if (state.stage === 'ready') break;
    await pause(100);
  }
  assert.equal((await evaluate('globalThis.__deviceBackupSmoke.state()')).stage, 'ready');
  const source = path.join(directory, 'entry-count.bin');
  const file = await open(source, 'wx');
  const database = Buffer.concat([Buffer.from([0, 82, 73, 83, 85, 83, 65, 86, 69, 0, 7]),
    pack({ characters: [], username: 'Synthetic entry count fixture' })]);
  try {
    await file.writeFile(frame('database.risudat', database));
    for (let index = 0; index < count; index += 1024) {
      const frames = [];
      for (let item = index; item < Math.min(index + 1024, count); item++) frames.push(frame(name(item), body(item)));
      await file.writeFile(Buffer.concat(frames));
    }
  } finally { await file.close(); }
  const nativeSource = await prepareSource(source);
  const result = { schema: 'risunest.synthetic-legacy-entry-count/v1', synthetic: true, count,
    nameLength, bodyLength, totalNameBytes: count * nameLength + 16, phase: 'prepared' };
  const opened = await invoke('pds_open');
  const before = await memory();
  result.nativeBaselineBytes = before.nativeWorkingSetBytes;
  result.nativeMaxBytes = before.nativeWorkingSetBytes;
  result.nativePeakBytes = before.nativePeakWorkingSetBytes;
  result.rendererMaxBytes = before.usedBytes;
  const started = Date.now();
  const job = await invoke('native_file_job_start', { request: {
    kind: 'restore-legacy-local-backup', source: { type: 'desktopPath', path: nativeSource }, expectedRevision: opened.revision,
  } });
  let finalized = false;
  let status;
  do {
    status = await invoke('native_file_job_status', { jobId: job.jobId });
    const sample = await memory();
    result.nativeMaxBytes = Math.max(result.nativeMaxBytes, sample.nativeWorkingSetBytes);
    result.nativePeakBytes = Math.max(result.nativePeakBytes, sample.nativePeakWorkingSetBytes);
    result.rendererMaxBytes = Math.max(result.rendererMaxBytes, sample.usedBytes);
    result.phase = status.phase;
    result.elapsedMs = Date.now() - started;
    await save(result);
    if (status.phase === 'awaiting-activation' && !finalized) {
      await invoke('native_file_job_finalize', { jobId: job.jobId, expectedRevision: opened.revision });
      finalized = true;
    }
    if (result.elapsedMs > 60 * 60_000) throw new Error('Synthetic entry restore timed out');
  } while (!['succeeded', 'failed', 'cancelled'].includes(status.state));
  assert.equal(status.state, 'succeeded', JSON.stringify(status.error));
  result.nativeIncrementBytes = result.nativeMaxBytes - result.nativeBaselineBytes;
  result.phase = 'readback';
  await save(result);
  let cursor;
  let verified = 0;
  do {
    const page = await invoke('pds_list_asset_aliases', { query: { kind: 'asset', limit: 512, cursor } });
    for (let start = 0; start < page.items.length; start += 16) {
      const items = page.items.slice(start, start + 16);
      const expected = items.map((item, offset) => {
        const index = verified + offset;
        assert.equal(item.key, `assets/${name(index)}`);
        assert.equal(item.size, bodyLength);
        assert.equal(item.objectHash, digest(body(index)));
        return { contentHash: item.objectHash, start: 0, endExclusive: bodyLength, expectedSize: bodyLength };
      });
      const values = await evaluate(`Promise.all(${JSON.stringify(expected)}.map(args=>globalThis.__TAURI_INTERNALS__.invoke('asset_cas_read_bytes',args).then(bytes=>Array.from(new Uint8Array(bytes)))))`);
      for (let offset = 0; offset < values.length; offset++) assert.deepEqual(Buffer.from(values[offset]), body(verified + offset));
      verified += items.length;
    }
    cursor = page.nextCursor;
    result.verified = verified;
    await save(result);
  } while (cursor);
  assert.equal(verified, count);
  await invoke('native_file_job_forget', { jobId: job.jobId });
  const rootBefore = await invoke('pds_read_root');
  const damaged = path.join(directory, 'truncated.bin');
  await writeFile(damaged, Buffer.concat([frame('database.risudat', database), frame('truncated', Buffer.alloc(32)).subarray(0, 20)]));
  const nativeDamaged = await prepareSource(damaged);
  const failed = await invoke('native_file_job_start', { request: {
    kind: 'restore-legacy-local-backup', source: { type: 'desktopPath', path: nativeDamaged }, expectedRevision: rootBefore.revision,
  } });
  do {
    status = await invoke('native_file_job_status', { jobId: failed.jobId });
    if (!['succeeded', 'failed', 'cancelled'].includes(status.state)) await pause(100);
  } while (!['succeeded', 'failed', 'cancelled'].includes(status.state));
  assert.equal(status.state, 'failed');
  assert.deepEqual(await invoke('pds_read_root'), rootBefore);
  await invoke('native_file_job_forget', { jobId: failed.jobId });
  result.rollbackVerified = true;
  result.phase = 'verified';
  await save(result);
  return result;
}
