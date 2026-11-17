import { existsSync, readFileSync } from 'node:fs'
import { relative, resolve } from 'node:path'
import { defineConfig, mergeConfig } from 'vitest/config'
import { sharedVitestConfig } from '../../tests/vitestShared'

const root = process.cwd()
const baseline = process.env.OUTPUT_LISTENER_BASELINE_SOURCE
export default mergeConfig(sharedVitestConfig(), defineConfig({
    plugins: [{
        // Loads preserved product sources in place of the working tree to measure an earlier implementation.
        name: 'output-listener-baseline-source',
        enforce: 'pre',
        load(id) {
            if (!baseline) return
            const path = relative(root, id.split('?')[0]).replaceAll('\\', '/')
            if (!path.startsWith('src/')) return
            const snapshot = resolve(baseline, path)
            if (existsSync(snapshot)) return readFileSync(snapshot, 'utf8')
        },
    }],
    test: { name: 'harness', include: ['benchmarks/output-listener-memory/listenerMemory.test.ts'], maxWorkers: 1 },
}))
