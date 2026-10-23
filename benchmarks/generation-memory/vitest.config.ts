import { readFileSync, existsSync } from 'node:fs'
import { relative, resolve } from 'node:path'
import { defineConfig, mergeConfig } from 'vitest/config'
import { sharedVitestConfig } from '../../tests/vitestShared'

const root = process.cwd()
const baseline = process.env.GENERATION_MEMORY_BASELINE_SOURCE
export default mergeConfig(sharedVitestConfig(), defineConfig({
    plugins: [{
        name: 'generation-memory-observation',
        enforce: 'pre',
        load(id) {
            if (!baseline) return
            const path = relative(root, id.split('?')[0]).replaceAll('\\', '/')
            if (!path.startsWith('src/')) return
            const snapshot = resolve(baseline, path)
            if (existsSync(snapshot)) return readFileSync(snapshot, 'utf8')
        },
        transform(code, id) {
            if (!id.replaceAll('\\', '/').endsWith('/src/ts/storage/saveCoordinator.ts')) return
            for (const name of ['messageMatchesAfterIdNormalization', 'conversationMatchesAfterIdNormalization', 'conversationJsonEquals']) {
                const pattern = new RegExp(`function ${name}\\([\\s\\S]*?\\): boolean \\{`)
                if (!pattern.test(code)) throw new Error(`Missing measured comparison ${name}`)
                code = code.replace(pattern, (declaration) => declaration + `\n    (globalThis as any).__generationMemoryComparison?.('${name}')`)
            }
            return { code, map: null }
        },
    }],
    test: { name: 'harness', env: { GENERATION_MEMORY_COMPARISONS: '1' }, include: ['benchmarks/generation-memory/generationMemory.test.ts'], maxWorkers: 1 },
}))
