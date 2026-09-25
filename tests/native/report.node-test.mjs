import { test } from 'node:test'
import assert from 'node:assert/strict'
import { verifyReport } from './report.mjs'
test('native reports reject stale, incomplete, wrong-phase and failed process evidence', () => {
    const report = { runId: 'synthetic', phase: 'read', success: true, cases: ['readback', 'restart-without-reseed'] }
    verifyReport(report, 'synthetic', 'read', 0)
    for (const changed of [{ runId: 'old' }, { phase: 'write' }, { success: false }, { cases: ['readback'] }]) {
        assert.throws(() => verifyReport({ ...report, ...changed }, 'synthetic', 'read', 0))
    }
    assert.throws(() => verifyReport(report, 'synthetic', 'read', 1))
})
