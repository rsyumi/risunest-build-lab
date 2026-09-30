import type { RisuPlugin } from './plugins.svelte'

export function reconcilePluginListUpdate(
    current: readonly RisuPlugin[],
    proposed: readonly RisuPlugin[],
): { installed: RisuPlugin[]; additions: RisuPlugin[] } {
    const names = new Set(current.map((plugin) => plugin.name))
    const additions: RisuPlugin[] = []
    for (const plugin of proposed) {
        if (names.has(plugin.name)) continue
        names.add(plugin.name)
        additions.push(plugin)
    }
    return { installed: [...current], additions }
}
