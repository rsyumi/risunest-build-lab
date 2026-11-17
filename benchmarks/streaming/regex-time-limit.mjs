import { chromium } from '@playwright/test'
import { mkdir, writeFile } from 'node:fs/promises'
import path from 'node:path'
import { REALM_BLOCKED_URL_PATTERNS } from '../../scripts/realmBlocklist.mjs'

const [before, after, output = '.tmp/regex-time-limit/results.json', workload = 'mixed'] = process.argv.slice(2)
if (!['mixed', 'cbs'].includes(workload)) throw new Error('Choose mixed or cbs workload')
for (const url of [before, after]) {
    if (!url || !['127.0.0.1', 'localhost'].includes(new URL(url).hostname)) {
        throw new Error('Pass before and after loopback URLs serving the agent streaming harness')
    }
}
const browser = await chromium.launch({ headless: true })
try {
    const page = await browser.newPage({ viewport: { width: 1100, height: 900 } })
    const cdp = await page.context().newCDPSession(page)
    await cdp.send('Network.enable')
    await cdp.send('Network.setBlockedURLs', { urls: REALM_BLOCKED_URL_PATTERNS })
    let exceptions = 0
    page.on('pageerror', () => exceptions++)
    const results = []
    for (const [variant, url] of [['before', before], ['after', after]]) {
        await page.goto(url, { timeout: 120_000 })
        await page.waitForFunction(() => window.__streamingSmoke?.marker === 'synthetic-v1', { timeout: 120_000 })
        results.push({ variant, result: await page.evaluate((workload) => window.__streamingSmoke.regexTimeLimit(undefined, undefined, undefined, undefined, workload), workload) })
    }
    const rowScaling = []
    for (const rows of [1, 10]) {
        rowScaling.push(await page.evaluate(({ rows, workload }) => window.__streamingSmoke.regexTimeLimit([200], [30720], 3, rows, workload), { rows, workload }))
    }
    const retainedHeapBytes = []
    for (let repeat = 0; repeat < 5; repeat++) {
        await page.evaluate((workload) => window.__streamingSmoke.regexTimeLimit([200], [30720], 1, undefined, workload), workload)
        await cdp.send('HeapProfiler.collectGarbage')
        retainedHeapBytes.push((await cdp.send('Runtime.getHeapUsage')).usedSize)
    }
    await page.reload()
    await page.waitForFunction(() => window.__streamingSmoke?.marker === 'synthetic-v1')
    const timeout = await page.evaluate(() => window.__streamingSmoke.regexTimeout())
    if (!timeout.unchanged || timeout.firstMs > 25_000 || timeout.repeatMs > 1000 || timeout.timerMs > 500 || exceptions) {
        throw new Error('Regex timeout responsiveness or rendering failed')
    }
    await mkdir(path.dirname(output), { recursive: true })
    await writeFile(output, JSON.stringify({ results, rowScaling, retainedHeapBytes, timeout, exceptions }, null, 2))
    console.log(JSON.stringify({ output, timeout, exceptions }))
} finally { await browser.close() }
