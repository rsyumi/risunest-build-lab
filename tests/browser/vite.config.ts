import { defineConfig } from 'vite'
import { svelte } from '@sveltejs/vite-plugin-svelte'
import { resolve } from 'node:path'

const adapters = new Set(["../plugins.svelte", "src/ts/storage/database.svelte", "src/ts/storage/workingSetCatalog", "../pluginSafeClass", "../pluginClaimSession", "src/ts/stores.svelte", "src/ts/util", "src/ts/alert", "src/lang", "src/ts/globalApi.svelte", "src/ts/gui/colorscheme", "src/ts/platform", "src/ts/process/mcp/pluginmcp", "src/ts/process/files/inlays", "src/ts/translator/translator", "src/ts/parser/parser.svelte", "src/ts/storage/nativePluginPermissions", "src/ts/process/index.svelte", "src/ts/process/generationState", "src/ts/model/modellist", "src/ts/process/request/request", "src/ts/process/modules", "src/ts/process/ttsHooks", "src/ts/storage/persistentDataRuntime.svelte", "src/ts/conversationMutations", "../pluginDatabaseAccess"])
export default defineConfig(({ mode }) => {
    if (mode !== 'agent') throw new Error('Browser fixture requires agent mode')
    return {
        root: resolve(import.meta.dirname),
        plugins: [{
            name: 'fixture-host-boundaries', enforce: 'pre',
            async resolveId(source, importer, options) {
                source = source.replaceAll('\\', '/').replace(resolve(import.meta.dirname, '../../src').replaceAll('\\', '/') + '/', 'src/')
                if (importer?.replaceAll('\\', '/').endsWith('/src/ts/plugins/apiV3/v3.svelte.ts') && adapters.has(source)) {
                    return resolve(import.meta.dirname, 'hostAdapters.ts').replaceAll('\\', '/')
                }
                if (!/realmEndpoints(\.ts)?$/.test(source)) return null
                const resolved = await this.resolve(source, importer, { ...options, skipSelf: true })
                if (!resolved?.id.replaceAll('\\', '/').endsWith('/src/ts/realmEndpoints.ts')) return null
                return resolved.id.replace(/realmEndpoints\.ts$/, 'realmEndpoints.blocked.ts')
            },
        }, svelte()],
        resolve: { alias: { src: resolve(import.meta.dirname, '../../src') } },
        server: { hmr: false, host: '127.0.0.1', port: 4187, strictPort: true, fs: { allow: [resolve(import.meta.dirname, '../..')] } },
        build: { outDir: resolve(import.meta.dirname, '../../.tmp/test-results/browser/dist'), emptyOutDir: true },
    }
})
