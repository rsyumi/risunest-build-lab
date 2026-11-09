import { createHash } from 'node:crypto'
import { readFileSync } from 'node:fs'
import { resolve } from 'node:path'
import { defineConfig, mergeConfig } from 'vite'
import executionConfig from '../execution-profile/vite.config'
import { repositoryRoot } from '../execution-profile/probe.mjs'

export default defineConfig(async environment => {
    if (environment.command !== 'build' || environment.mode !== 'agent') throw new Error('ollama-ui:agent-build-required')
    const base = await (typeof executionConfig === 'function' ? executionConfig(environment) : executionConfig)
    const files = [
        'src/main.ts', 'src/App.svelte', 'src/ts/process/nativeOllamaRoute.ts',
        'src/lib/ChatScreens/DefaultChatScreen.svelte', 'src/ts/storage/sqlitePersistentDataStore.ts',
        'tests/native/ollama-route/bootstrap.mjs', 'tests/native/ollama-route/commandWitness.mjs',
        'tests/native/ollama-route/vite.config.ts', 'node_modules/@tauri-apps/api/core.js',
    ]
    const sourceHashes = Object.fromEntries(files.map(file => [file,
        createHash('sha256').update(readFileSync(resolve(repositoryRoot, file))).digest('hex')]))
    const build = { schema: 'risunest.ollama-ui/v1', entry: 'src/main.ts', mode: 'agent', sourceHashes,
        identity: createHash('sha256').update(JSON.stringify(sourceHashes)).digest('hex') }
    let invocationObserved = 0
    return mergeConfig(base, {
        define: { __OLLAMA_ROUTE_BUILD__: JSON.stringify(build) },
        build: { outDir: resolve(repositoryRoot, '.tmp/test-results/ollama-route/dist') },
        plugins: [{
            name: 'risunest-ollama-ui-startup-gate',
            enforce: 'pre',
            transform(code, id) {
                if (!id.split('?')[0].replaceAll('\\', '/').endsWith('/@tauri-apps/api/core.js')) return null
                const original = 'return window.__TAURI_INTERNALS__.invoke(cmd, args, options);'
                if (code.split(original).length !== 2) throw new Error('ollama-ui:invoke-boundary-changed')
                if (createHash('sha256').update(code).digest('hex') !== sourceHashes['node_modules/@tauri-apps/api/core.js']) {
                    throw new Error('ollama-ui:invoke-boundary-source-mismatch')
                }
                invocationObserved += 1
                return { code: code.replace(original, 'return globalThis.__risunestOllamaUi.witness.invoke(cmd, args, options);'), map: null }
            },
            transformIndexHtml: { order: 'pre', handler(html) {
                const original = '<script type="module" src="/src/main.ts"></script>'
                if (html.split(original).length !== 2) throw new Error('ollama-ui:product-entry-changed')
                return html.replace(original, '<script type="module" src="/tests/native/ollama-route/bootstrap.mjs"></script>')
            } },
            generateBundle() {
                if (invocationObserved !== 1) throw new Error('ollama-ui:invoke-boundary-not-observed-once')
                for (const [file, expected] of Object.entries(sourceHashes)) {
                    if (createHash('sha256').update(readFileSync(resolve(repositoryRoot, file))).digest('hex') !== expected) {
                        throw new Error('ollama-ui:build-source-changed')
                    }
                }
                this.emitFile({ type: 'asset', fileName: 'ollama-route-build.json', source: JSON.stringify(build, null, 2) })
            },
        }],
    })
})

