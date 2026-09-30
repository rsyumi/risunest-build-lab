import { readFileSync } from 'node:fs'
import path from 'node:path'
import { defineConfig, mergeConfig } from 'vite'
import product from '../../vite.config'

export default defineConfig(async (environment) => {
    if (environment.mode !== 'agent') throw new Error('Plugin probe requires agent mode')
    const root = path.resolve(import.meta.dirname, '../..')
    const directory = path.join(root, 'benchmarks/plugin-review')
    const actualCore = path.join(root, 'node_modules/@tauri-apps/api/core.js').replaceAll('\\', '/')
    const base = typeof product === 'function' ? await product(environment) : product
    return mergeConfig(base, {
        root,
        plugins: [{
            name: 'synthetic-plugin-review', enforce: 'pre',
            resolveId(id) { if (id === '@tauri-apps/api/core') return '\0plugin-review-core' },
            load(id) {
                if (id !== '\0plugin-review-core') return
                return `export * from ${JSON.stringify(actualCore)};
                    import { invoke as nativeInvoke } from ${JSON.stringify(actualCore)};
                    export function invoke(command, args, options) {
                        globalThis.__pluginReviewCountInvoke?.(command);
                        return nativeInvoke(command, args, options);
                    }`
            },
            transformIndexHtml: { order: 'pre', handler: () => readFileSync(path.join(directory, 'index.html'), 'utf8') },
        }],
        build: { outDir: path.join(directory, 'dist'), emptyOutDir: true,
            rolldownOptions: { input: path.join(root, 'index.html') } },
    })
})
