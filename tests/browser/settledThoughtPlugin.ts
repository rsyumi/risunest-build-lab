import { resolve } from 'node:path'
import type { Plugin } from 'vite'

const root = resolve(import.meta.dirname, '../..').replaceAll('\\', '/')
const adapter = resolve(import.meta.dirname, 'settledThoughtAdapters.ts').replaceAll('\\', '/')
const boundaries = new Set([
    'database.svelte', 'stores.svelte', 'globalApi.svelte', 'platform',
    'chatVar.svelte', 'scripts', 'infunctions', 'util', 'inlays',
    'inlayRenderSource', 'modules', 'lang', 'modellist', 'cbs', 'alert', 'translator',
])

export function settledThoughtBoundaries(): Plugin {
    return {
        name: 'synthetic-settled-thought-boundaries', enforce: 'pre',
        resolveId(source, importer) {
            const owner = importer?.replaceAll('\\', '/')
            if (owner !== `${root}/src/ts/parser/parser.svelte.ts`
                && owner !== `${root}/src/lib/ChatScreens/ChatBody.svelte`) return null
            const file = source.replaceAll('\\', '/').split('/').at(-1)!
            return boundaries.has(file) ? adapter : null
        },
    }
}
