import { resolve } from 'node:path'
import type { Plugin } from 'vite'

export function sidebarProductBoundaries(): Plugin {
    return {
        name: 'sidebar-product-fixture-boundaries', enforce: 'pre',
        resolveId(source, importer) {
            const owner = importer?.replaceAll('\\', '/')
            const adapter = resolve(import.meta.dirname, 'sidebarProductAdapters.svelte.ts').replaceAll('\\', '/')
            if (owner?.endsWith('/src/lib/SideBars/SidebarAvatar.svelte') && source.endsWith('/tooltip')) return adapter
            if (!owner?.endsWith('/src/lib/SideBars/Sidebar.svelte')) return null
            if (/(stores\.svelte|database\.svelte|characters|lang|util|globalApi\.svelte|alert|guisize|workingSetNavigation)$/.test(source)) return adapter
            if (/\.svelte$/.test(source) && !/(stores|database)\.svelte$/.test(source)) {
                return /Sidebar(Avatar|Indicator)\.svelte$/.test(source) ? null : resolve(import.meta.dirname, 'Empty.svelte').replaceAll('\\', '/')
            }
            return null
        },
    }
}
