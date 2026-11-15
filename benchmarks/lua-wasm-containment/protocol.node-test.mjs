import assert from 'node:assert/strict';
import test from 'node:test';
import { classify, engineConfig } from './protocol.mjs';

const identity = { backend: 'cranelift', guestSha256: 'a'.repeat(64) };
function records(name = 'nested-pcall') {
    return [
        { phase: 'entered', case: name, lua: '5.4.9', wasmtime: '48.0.7', engineConfig, ...identity },
        { phase: 'complete', case: name, status: 'passed', outcome: 'fuel', elapsedMs: 1 },
    ];
}
function result(rows, overrides = {}, name = 'nested-pcall') {
    return classify(name, identity, { output: rows.map((row) => typeof row === 'string' ? row : JSON.stringify(row)).join('\n'),
        code: 0, ...overrides });
}

test('only complete matching runtime evidence passes', () => {
    assert.equal(result(records()).status, 'passed');
    for (const mutate of [
        (rows) => rows.slice(0, 1),
        (rows) => rows.slice(1),
        (rows) => [...rows, rows[1]],
        (rows) => [rows[0], rows[0], rows[1]],
        (rows) => [{ ...rows[0], guestSha256: 'b'.repeat(64) }, rows[1]],
        (rows) => [{ ...rows[0], backend: 'pulley' }, rows[1]],
        (rows) => [{ ...rows[0], engineConfig: 'unbounded' }, rows[1]],
        (rows) => [{ ...rows[0], lua: '5.5.1' }, rows[1]],
        (rows) => [{ ...rows[0], wasmtime: '48.0.1' }, rows[1]],
        (rows) => [rows[0], { ...rows[1], case: 'compiler' }],
        (rows) => [rows[0], { ...rows[1], outcome: 'engine-trap' }],
        (rows) => [rows[0], { ...rows[1], elapsedMs: -1 }],
        (rows) => [rows[0], { ...rows[1], unexplained: true }],
        () => ['not json'],
        () => ['[]'],
        () => [],
    ]) assert.equal(result(mutate(records())).status, 'protocol-failed');
});

test('exit and watchdog failures cannot be turned into passes by stdout', () => {
    assert.equal(result(records(), { code: 1 }).status, 'probe-failed');
    assert.equal(result(records(), { signal: 'SIGKILL' }).status, 'probe-failed');
    assert.equal(result(records(), { watchdog: true }).status, 'containment-failed');
    assert.equal(result(['invalid'], { watchdog: true }).status, 'containment-failed');
    assert.equal(result(records(), { outputLimit: true }).status, 'output-limit-failed');
});

test('unsupported async context is explicitly classified as a limitation', () => {
    const rows = records('non-yieldable-await');
    Object.assign(rows[1], { status: 'limitation-confirmed', outcome: 'non-yieldable-await-rejected' });
    assert.equal(result(rows, {}, 'non-yieldable-await').status, 'limitation-confirmed');
    rows[1].status = 'passed';
    assert.equal(result(rows, {}, 'non-yieldable-await').status, 'protocol-failed');
});

test('epoch witnesses reject an ordinary fuel or deadline label', () => {
    for (const name of ['deadline-native-pattern', 'deadline-gc-finalizer']) {
        const rows = records(name);
        for (const outcome of ['fuel', 'deadline', 'cancelled', 'engine-trap']) {
            rows[1].outcome = outcome;
            assert.equal(result(rows, {}, name).status, 'protocol-failed');
        }
        rows[1].outcome = 'epoch-deadline';
        assert.equal(result(rows, {}, name).status, 'passed');
    }
});
