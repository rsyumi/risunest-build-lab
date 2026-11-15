import assert from 'node:assert/strict';
import { test } from 'node:test';
import { mkdtemp, rm, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { classifyProbeResult } from './protocol.mjs';
import { runCases } from './run.mjs';

const entered = { phase: 'entered', case: 'retention', lua: '5.4.9', mlua: '0.12.2' };
const complete = { phase: 'complete', case: 'retention', status: 'passed', elapsedMs: 1 };
const output = (...rows) => rows.map((row) => JSON.stringify(row)).join('\n');
const classify = (text, processResult = {}) => classifyProbeResult('retention', { output: text, code: 0, ...processResult });

test('requires matching identity, ordered complete records and successful process exit', () => {
    assert.equal(classify(output(entered, complete)).status, 'passed');
    for (const [name, text, processResult] of [
        ['wrong entered case', output({ ...entered, case: 'await' }, complete)],
        ['wrong complete case', output(entered, { ...complete, case: 'await' })],
        ['both cases wrong', output({ ...entered, case: 'await' }, { ...complete, case: 'await' })],
        ['missing entered', output(complete)],
        ['missing complete', output(entered)],
        ['missing all', ''],
        ['duplicate entered', output(entered, entered, complete)],
        ['duplicate complete', output(entered, complete, complete)],
        ['record after completion', output(entered, complete, { phase: 'dispose', case: 'retention' })],
        ['duplicate dispose', output(entered, ...Array(2).fill({ phase: 'dispose', case: 'retention' }), complete)],
        ['unknown phase', output(entered, { phase: 'failed', case: 'retention' }, complete)],
        ['contradictory completion', output(entered, { ...complete, status: 'failed' })],
        ['contradictory exit code', output(entered, complete), { code: 1 }],
        ['signal after completion', output(entered, complete), { signal: 'SIGTERM' }],
        ['missing elapsed time', output(entered, { ...complete, elapsedMs: undefined })],
        ['invalid elapsed time', output(entered, { ...complete, elapsedMs: -1 })],
        ['unexpected version', output({ ...entered, lua: '5.5.1' }, complete)],
        ['unexpected status field', output({ ...entered, status: 'failed' }, complete)],
        ['malformed JSON', output(entered) + '\n{'],
        ['nonobject JSON', 'null'],
    ]) {
        const result = classify(text, processResult);
        assert.equal(result.status, 'failed', name);
        assert.ok(result.protocolError, name);
    }
});

test('keeps watchdog detection separate from successful runtime containment', () => {
    const detected = classify(output(entered, { phase: 'dispose', case: 'retention' }), { code: null, signal: 'SIGKILL', watchdog: true });
    assert.equal(detected.status, 'containment-failed');
    assert.equal(detected.phase, 'dispose');
    assert.equal(detected.elapsedMs, null);
    assert.equal(classify(output(entered, complete), { code: null, signal: 'SIGKILL', watchdog: true }).status, 'containment-failed');
    assert.equal(classify(output({ ...entered, case: 'await' }), { watchdog: true }).status, 'failed');
});

test('malformed child output fails only its case and the real runner continues', async () => {
    const directory = await mkdtemp(join(tmpdir(), 'lua-probe-protocol-'));
    try {
        const fixture = join(directory, 'child.mjs');
        await writeFile(fixture, [
            "const name=process.argv.at(-1);",
            "console.log(JSON.stringify({phase:'entered',case:name,lua:'5.4.9',mlua:'0.12.2'}));",
            "if(name==='retention') console.log('{');",
            "else console.log(JSON.stringify({phase:'complete',case:name,status:'passed',elapsedMs:0}));",
        ].join('\n'));
        const delivered = [];
        const result = await runCases(process.execPath, ['retention', 'await'], {
            prefixArguments: [fixture],
            onResult: (row) => delivered.push(row),
        });
        assert.equal(result.length, 2);
        assert.equal(result[0].status, 'failed');
        assert.match(result[0].protocolError, /invalid JSON/);
        assert.equal(result[1].status, 'passed');
        assert.deepEqual(delivered, result);
    } finally {
        await rm(directory, { recursive: true, force: true });
    }
});
