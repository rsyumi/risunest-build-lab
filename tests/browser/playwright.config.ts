import { defineConfig } from '@playwright/test'
export default defineConfig({
    testDir: '.', testMatch: '**/*.browser.spec.ts', workers: 1, retries: 0,
    outputDir: '../../.tmp/test-results/browser/artifacts',
    projects: [
        { name: 'chromium', use: { browserName: 'chromium' } },
        { name: 'webkit', use: { browserName: 'webkit' }, testMatch: ['**/pluginIframe.browser.spec.ts', '**/largeThoughtExpansion.browser.spec.ts', '**/modalNavigation.browser.spec.ts', '**/lazyApp.browser.spec.ts'] },
    ],
    use: { headless: true, baseURL: 'http://127.0.0.1:4187', serviceWorkers: 'block' },
    webServer: { command: 'pnpm exec vite build --mode agent --config vite.config.ts && pnpm exec vite preview --mode agent --config vite.config.ts --host 127.0.0.1 --port 4187 --strictPort', url: 'http://127.0.0.1:4187', reuseExistingServer: false },
})
