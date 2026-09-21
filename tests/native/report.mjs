import assert from 'node:assert/strict'
const expectedCases = {
    write: ['commit', 'readback', 'stale-rejected'],
    read: ['readback', 'restart-without-reseed'],
    abort: ['commit', 'readback', 'abort-released'],
    'read-abort': ['readback', 'restart-without-reseed'],
}
export function verifyReport(report, runId, phase, status) {
    assert.ok(Object.hasOwn(expectedCases, phase), 'unknown phase')
    assert.equal(status, 0, report.error ?? 'native process exit')
    assert.equal(report.runId, runId, 'run identity')
    assert.equal(report.phase, phase, 'process phase')
    assert.equal(report.success, true, report.error ?? 'native assertions failed')
    assert.deepEqual(report.cases, expectedCases[phase])
}
