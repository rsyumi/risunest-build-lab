import { readFileSync } from 'node:fs'
import { test as base, expect } from '@playwright/test'
import { classifyTestRequest } from '../support/testNetwork'
import type { BrowserDriver } from './main'
import type { thoughtDriver } from './thoughtDriver'
declare global { interface Window { boundary: BrowserDriver; thought: typeof thoughtDriver } }
export const test = base.extend<{ guarded: void }>({
    guarded: [async ({ context, baseURL }, use) => {
        const rejected: string[] = []
        const origins = new Set([new URL(baseURL!).origin])
        await context.route('**/*', async route => {
            const reason = classifyTestRequest(route.request().url(), origins)
            // Serve our owned HTML directly so machine-level HTTP injectors cannot add scripts.
            const pathname = new URL(route.request().url()).pathname
            if (!reason && (pathname === '/' || pathname === '/dragDrop.html' || pathname === '/modalNavigation.html' || pathname === '/lazyApp.html')) {
                const file = pathname === '/' ? 'index.html' : pathname.slice(1)
                await route.fulfill({ contentType: 'text/html', body: readFileSync(new URL(`../../.tmp/test-results/browser/dist/${file}`, import.meta.url), 'utf8') })
                return
            }
            if (reason) { rejected.push(`${reason}: ${route.request().url()}`); await route.abort() } else await route.continue()
        })
        await use()
        expect(rejected).toEqual([])
    }, { auto: true }],
})
export { expect }
