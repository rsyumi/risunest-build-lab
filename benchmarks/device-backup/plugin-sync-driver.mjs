import assert from 'node:assert/strict';
import { spawn } from 'node:child_process';
import { once } from 'node:events';
import { copyFile, mkdir, writeFile } from 'node:fs/promises';
import net from 'node:net';
import path from 'node:path';
import { waitForPluginMeasurement } from './plugin-value-driver.mjs';

async function command(executable, args) {
  const child = spawn(executable, args, { windowsHide: true, stdio: ['ignore', 'pipe', 'pipe'] });
  let stdout = '';
  child.stdout.on('data', bytes => { stdout += bytes; });
  child.stderr.resume();
  const [code] = await once(child, 'exit');
  assert.equal(code, 0, 'Synthetic Sync command failed');
  return stdout;
}

function registration(raw, endpoint) {
  const uri = raw.match(/risunestlocal:\/\/\S+/g) ?? [];
  const objects = raw.split(/\r?\n/).filter(line => line.trim().startsWith('{'));
  const body = uri.length === 1 ? JSON.parse(Buffer.from(new URL(uri[0]).hash.slice(1), 'base64url').toString())
    : uri.length === 0 && objects.length === 1 ? JSON.parse(objects[0]) : null;
  assert.ok(body && ['libraryId', 'deviceId', 'token'].every(key => typeof body[key] === 'string'), 'Synthetic registration shape differs');
  return { endpoint, libraryId: body.libraryId, deviceId: body.deviceId, token: body.token };
}

export async function syntheticPluginSyncServer(executable, directory) {
  await mkdir(directory);
  const ownedExecutable = path.join(directory, path.basename(executable));
  await copyFile(executable, ownedExecutable);
  const data = path.join(directory, 'data');
  await command(ownedExecutable, ['init', '--data-dir', data]);
  const reservation = net.createServer();
  reservation.listen(0, '127.0.0.1');
  await once(reservation, 'listening');
  const port = reservation.address().port;
  const endpoint = `http://127.0.0.1:${port}`;
  const configs = [];
  for (const peer of ['sender', 'receiver']) {
    const raw = await command(ownedExecutable, ['device', 'add', '--data-dir', data]);
    await writeFile(path.join(directory, `${peer}.secret.txt`), raw);
    configs.push(registration(raw, endpoint));
  }
  await new Promise(resolve => reservation.close(resolve));
  const server = spawn(ownedExecutable, ['serve', '--data-dir', data, '--listen', `127.0.0.1:${port}`], { windowsHide: true, stdio: 'ignore' });
  for (let attempt = 0; ; attempt++) {
    if (server.exitCode !== null || attempt >= 100) throw new Error('Synthetic Sync server did not start');
    const listening = await new Promise(resolve => {
      const socket = net.connect(port, '127.0.0.1');
      socket.once('connect', () => { socket.destroy(); resolve(true); });
      socket.once('error', () => { socket.destroy(); resolve(false); });
      socket.setTimeout(500, () => { socket.destroy(); resolve(false); });
    });
    if (listening) break;
    await new Promise(resolve => setTimeout(resolve, 100));
  }
  return { port, configs, async stop() { if (server.exitCode === null) { server.kill(); await once(server, 'exit'); } } };
}

export async function measurePluginSync({ config, sender, evaluate, memory, save }) {
  await waitForPluginMeasurement(evaluate);
  const started = Date.now();
  // Credentials stay in this call and the private fixture files, never in the measurement report.
  await evaluate(`void (async () => {
    globalThis.__pluginSyncMeasurement = { phase: 'binding' };
    try {
      const invoke = globalThis.__TAURI_INTERNALS__.invoke;
      await invoke('pds_open');
      await invoke('pds_set_section_participating', { section: 'local-plugins', participating: true });
      await invoke('server_sync_configure', { config: ${JSON.stringify(config)} });
      const original = await invoke('pds_lww_binding_state');
      const header = () => ({ bindingAuthority: original.targetAuthority, requestId: crypto.randomUUID() });
      const inspected = await invoke('server_sync_lww_inspect', { request: header(), newDevice: false });
      if (inspected.empty !== ${sender}) throw new Error('Unexpected synthetic Sync state');
      const staged = inspected.empty ? null : await invoke('server_sync_lww_stage_target', { request: header(), inspectionId: inspected.inspectionId });
      await invoke('server_sync_lww_fence', { newDevice: false });
      const current = await invoke('pds_lww_switch_target', { request: { ...header(), expectedSelectionEpoch: original.selectionEpoch,
        target: { kind: 'server', connectionId: 'server' }, inspectionId: inspected.inspectionId, initialPublication: ${sender} } });
      if (staged) await invoke('pds_lww_replace_from_target', { request: { bindingAuthority: current.targetAuthority, requestId: staged.receiveId,
        expectedSelectionEpoch: current.selectionEpoch, stagingId: staged.stagingId, receiveId: staged.receiveId, targetId: staged.targetId, libraryId: staged.libraryId } });
      const request = () => ({ bindingAuthority: current.targetAuthority, requestId: crypto.randomUUID() });
      await invoke('server_sync_lww_activate', { request: request() });
      globalThis.__pluginSyncMeasurement = { phase: 'transferring' };
      if (${sender}) while (await invoke('server_sync_lww_push', { request: request(), generating: [] })) {}
      globalThis.__pluginSyncMeasurement = { phase: 'verified' };
    } catch (error) { globalThis.__pluginSyncMeasurement = { phase: 'failed', code: error?.code ?? error?.message ?? 'unknown' }; }
  })(); true`);
  const result = { sender, memory: [] };
  for (;;) {
    const state = await evaluate('globalThis.__pluginSyncMeasurement');
    result.state = state;
    result.elapsedMs = Date.now() - started;
    result.memory.push({ phase: state.phase, ...await memory() });
    await save(result);
    if (state.phase === 'failed') throw new Error(`Synthetic plugin Sync: ${state.code}`);
    if (state.phase === 'verified') return result;
    if (result.elapsedMs > 30 * 60_000) throw new Error('Synthetic plugin Sync timed out');
    await new Promise(resolve => setTimeout(resolve, 500));
  }
}
