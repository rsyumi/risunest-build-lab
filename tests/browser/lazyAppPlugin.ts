import { resolve } from 'node:path'
import type { Plugin } from 'vite'

const root = resolve(import.meta.dirname, '../..').replaceAll('\\', '/')
const fixture = resolve(import.meta.dirname).replaceAll('\\', '/')
const lazyScreens = new Map([
    ['Settings.svelte', 'settings'], ['GridCatalog.svelte', 'grid'], ['botpreset.svelte', 'presets'],
    ['listedPersona.svelte', 'personas'], ['CustomGUISettingMenu.svelte', 'custom'],
])
const retained = new Set(['MobileBody.svelte', 'MobileFooter.svelte', 'LazyScreenError.svelte', 'LoadingIndicator.svelte'])

export function lazyAppBoundaries(): Plugin {
    return {
        name: 'synthetic-app-lazy-route-boundaries', enforce: 'pre',
        transform(code, id) {
            const owner = id.replaceAll('\\', '/')
            if (owner !== `${root}/src/App.svelte` && !owner.endsWith('/src/lib/Mobile/MobileBody.svelte')) return null
            return code.replace(/(?<!typeof )import\('([^']+\.svelte)'\)/g, (expression, path: string) => {
                const route = lazyScreens.get(path.split('/').at(-1)!)
                return route ? `(window as any).lazyImportBoundary.load('${route}', () => ${expression})` : expression
            })
        },
        resolveId(source, importer) {
            const owner = importer?.replaceAll('\\', '/')
            if (!owner || !(owner === `${root}/src/App.svelte` || [...retained].some(file => owner.endsWith(`/${file}`)))) return null
            if (!(source.startsWith('.') || source.startsWith('src/') || source.replaceAll('\\', '/').startsWith(`${root}/src/`))) return null
            const file = source.split('/').at(-1)!
            const route = lazyScreens.get(file)
            if (route) return `\0lazy-app-${route}`
            if (source.endsWith('.svelte') && !/(?:^|\/)ts\//.test(source)) {
                if (retained.has(file)) return null
                return `${fixture}/${file === 'Sidebar.svelte' ? 'LazyAppSidebar.svelte' : 'Empty.svelte'}`
            }
            if (source === '@lucide/svelte' || source.endsWith('.mp3') || file === 'dragTypes') return null
            return `${fixture}/lazyAppAdapters.ts`
        },
        load(id) {
            if (!id.startsWith('\0lazy-app-')) return null
            return `export { default } from ${JSON.stringify(`${fixture}/LazyAppLoaded.svelte`)}; export const route = ${JSON.stringify(id.slice(10))};`
        },
    }
}
