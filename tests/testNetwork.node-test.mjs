import { test } from 'node:test'
import assert from 'node:assert/strict'
import { mkdtempSync, mkdirSync, writeFileSync, readFileSync, rmSync } from 'node:fs'
import { resolve, join, dirname } from 'node:path'
import { spawnSync } from 'node:child_process'
import { createRequire } from 'node:module'

const root = resolve(import.meta.dirname, '..')
test('real setup fails swallowed fetch, XHR and iframe requests, including restored mocks', () => {
    const base = join(root, '.tmp/test-results')
    mkdirSync(base, { recursive: true })
    const directory = mkdtempSync(join(base, 'network-policy-'))
    const path = value => JSON.stringify(value.replaceAll('\\', '/'))
    try {
        writeFileSync(join(directory, 'probe.test.ts'), `
import { it, expect, vi } from 'vitest'
it('caught fetch', async () => { try { await fetch('https://unexpected.invalid/a') } catch {} })
it('restored mock', async () => {
    vi.stubGlobal('fetch', vi.fn()); vi.unstubAllGlobals()
    try { await fetch(new Request('https://unexpected.invalid/b')) } catch {}
})
it('Realm', async () => { try { await fetch('https://sv.risuai.xyz/realm/synthetic') } catch {} })
it('XHR', async () => {
    await new Promise(resolve => {
        const xhr = new XMLHttpRequest()
        xhr.onerror = () => resolve(undefined)
        xhr.open('GET', 'https://unexpected.invalid/xhr'); xhr.send()
    })
})
it('iframe', async () => {
    const frame = document.createElement('iframe')
    try {
        await new Promise(resolve => {
            frame.onerror = () => resolve(undefined)
            frame.src = 'https://unexpected.invalid/frame'; document.body.append(frame)
        })
    } finally { frame.remove() }
})
it('clean following test', () => { expect(true).toBe(true) })
`)
        writeFileSync(join(directory, 'vitest.config.mjs'), `export default { test: {
root: ${path(root)}, environment: 'happy-dom', setupFiles: [${path(join(root, 'vitest.setup.ts'))}],
include: [${path(join(directory, 'probe.test.ts'))}], exclude: [],
reporters: ['json'], outputFile: ${path(join(directory, 'report.json'))}
} }`)
        const require = createRequire(import.meta.url)
        const result = spawnSync(process.execPath, [join(dirname(require.resolve('vitest/package.json')), 'vitest.mjs'), 'run', '--config', join(directory, 'vitest.config.mjs')], {
            cwd: root, encoding: 'utf8', timeout: 60_000, windowsHide: true,
        })
        assert.equal(result.error, undefined)
        assert.equal(result.status, 1)
        const report = JSON.parse(readFileSync(join(directory, 'report.json'), 'utf8'))
        const cases = report.testResults.flatMap(file => file.assertionResults)
        assert.equal(cases.length, 6)
        for (const entry of cases.slice(0, 5)) {
            assert.equal(entry.status, 'failed', entry.title)
            assert.match(entry.failureMessages.join(' '), /Blocked test requests/, entry.title)
        }
        assert.equal(cases[5].status, 'passed')
    } finally {
        assert.ok(directory.startsWith(base + '/network-policy-') || directory.startsWith(base + '\\network-policy-'))
        rmSync(directory, { recursive: true, force: true })
    }
})
