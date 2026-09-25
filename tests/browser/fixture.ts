import { readFileSync } from 'node:fs'
import { test as base, expect } from '@playwright/test'
import { classifyTestRequest } from '../support/testNetwork'
import type { BrowserDriver } from './main'
import type { thoughtDriver } from './thoughtDriver'
declare global { interface Window { boundary: BrowserDriver; thought: typeof thoughtDriver } }
export const test = base.extend<{ guarded: void }>({
    guarded: [async ({ context }, use) => {
        const rejected: string[] = []
        await context.route('**/*', async route => {
            const reason = classifyTestRequest(route.request().url(), new Set(['http://127.0.0.1:4187']))
            // Serve our owned HTML directly so machine-level HTTP injectors cannot add scripts.
            if (!reason && new URL(route.request().url()).pathname === '/') {
                await route.fulfill({ contentType: 'text/html', body: readFileSync(new URL('../../.tmp/test-results/browser/dist/index.html', import.meta.url), 'utf8') })
                return
            }
            if (reason) { rejected.push(`${reason}: ${route.request().url()}`); await route.abort() } else await route.continue()
        })
        await use()
        expect(rejected).toEqual([])
    }, { auto: true }],
})
export { expect }
