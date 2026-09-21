import { defineConfig } from '@playwright/test'
export default defineConfig({
    testDir: '.', testMatch: '**/*.browser.spec.ts', workers: 1, retries: 0,
    outputDir: '../../.tmp/test-results/browser/artifacts',
    use: { browserName: 'chromium', headless: true, baseURL: 'http://127.0.0.1:4187', serviceWorkers: 'block' },
    webServer: { command: 'pnpm exec vite build --mode agent --config vite.config.ts && pnpm exec vite preview --mode agent --config vite.config.ts --host 127.0.0.1 --port 4187 --strictPort', url: 'http://127.0.0.1:4187', reuseExistingServer: false },
})
