import assert from 'node:assert/strict';
import { mkdtemp, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import test from 'node:test';
import { nativeClient } from './native-client.mjs';

function child(t, source) {
  const client = nativeClient(process.execPath, ['-e', source]);
  t.after(() => client.close());
  return client;
}

const echo = `
  require('node:readline').createInterface({input: process.stdin}).on('line', line => {
    process.stdout.write(JSON.stringify({output: JSON.parse(line)}) + '\\n');
  });
`;

test('missing executable rejects pending and future sends cleanly', { timeout: 5000 }, async t => {
  const directory = await mkdtemp(join(tmpdir(), 'rust-cbs-client-'));
  t.after(() => rm(directory, { recursive: true, force: true }));
  for (const beforeFirstSend of [false, true]) {
    const client = nativeClient(join(directory, 'missing-executable'));
    t.after(() => client.close());
    if (beforeFirstSend) await new Promise(resolve => setImmediate(resolve));
    await assert.rejects(client.send({}), { code: 'ENOENT' });
    await assert.rejects(client.send({}), { code: 'ENOENT' });
  }
});

test('real child exchanges successive JSONL requests', { timeout: 5000 }, async t => {
  const client = child(t, echo);
  assert.deepEqual(await client.send({ text: 'first\nsecond', units: [0xd800] }),
    { output: { text: 'first\nsecond', units: [0xd800] } });
  assert.deepEqual(await client.send({ id: 2 }), { output: { id: 2 } });
  const effects = { status: 'evaluated', result: { error: { name: 'CbsHistoryAccessError', message: [0xd800], access: [] } },
    effects: [{ sequence: 0, key: { kind: 'undefined' }, value: { kind: 'undefined' } },
      { sequence: 1, key: { kind: 'text', units: [] }, value: { kind: 'text', units: [0x75, 0x6e] } }],
    history: { provenance: { captureId: [0xd800], characterId: [], conversationId: [0xdc00], revision: '18446744073709551615' },
      count: true, rows: [9007199254740990, 3] } };
  assert.deepEqual(await client.send(effects), { output: effects });
  const program = { op: 'evaluateEffects', text: [0xd800], input: { mode: 'mutableMap', program: {
    mode: 'localFunctions', limits: { functions: 16, frames: 24, sourceUnits: 4096, expandedUnits: 16384, retainedBytes: 65536 }
  } }, fold: true };
  assert.deepEqual(await client.send(program), { output: program });
});

test('invalid JSON rejects pending and future sends', { timeout: 5000 }, async t => {
  const client = child(t, `process.stdin.once('data', () => process.stdout.write('not-json\\n'));`);
  await assert.rejects(client.send({}), SyntaxError);
  await assert.rejects(client.send({}), SyntaxError);
});

test('early child exit retains process_exit for later sends', { timeout: 5000 }, async t => {
  const client = child(t, 'process.exit(0)');
  assert.deepEqual(await client.send({}), { error: 'process_exit' });
  assert.deepEqual(await client.send({}), { error: 'process_exit' });
});

test('timeout settles once and remains terminal', { timeout: 5000 }, async t => {
  const client = child(t, `process.stdin.on('data', () => {});`);
  assert.deepEqual(await client.send({}, 100), { error: 'timeout' });
  assert.deepEqual(await client.send({}), { error: 'timeout' });
  client.close();
  client.close();
});

test('child exit during a large stdin write rejects pending and future writes', { timeout: 5000 }, async t => {
  const client = child(t, `process.stdin.once('data', () => process.exit(0));`);
  await assert.rejects(client.send({ text: 'x'.repeat(1_048_576) }));
  await assert.rejects(client.send({}));
});
