import type { Plugin, Rollup } from 'vite'

// A redirected product module becomes a virtual module with every export name of the real module.
// Names the adapter exports come from the adapter; the rest throw, naming the export, once the fixture reaches them.
// A stub is always truthy, so a flag read in a condition must come from the adapter or the real module.
const prefix = '\0fixture-stub:'

// Pass the specifier exactly as imported: skipSelf skips only that specifier, so a rewritten one re-enters the caller.
// The keys in flight travel with the nested resolve, so a redirect that loops fails at once instead of exhausting memory.
export async function stubbed(context: Rollup.PluginContext, adapter: string, source: string, importer: string, options: Parameters<Rollup.PluginContext['resolve']>[2]) {
    const key = `${source} from ${importer}`
    const pending: string[] = options?.custom?.[prefix] ?? []
    if (pending.includes(key)) throw new Error(`Browser fixture redirect re-entered while resolving ${key}`)
    const real = await context.resolve(source, importer, { ...options, skipSelf: true, custom: { ...options?.custom, [prefix]: [...pending, key] } })
    if (!real) throw new Error(`Browser fixture cannot resolve ${key}`)
    // skipSelf skips only the calling plugin, so another fixture redirect may already have stubbed this import.
    return real.id.startsWith(prefix) ? real.id : `${prefix}${real.id}|${adapter}`
}

export function autoStubs(): Plugin {
    return {
        name: 'fixture-auto-stubs',
        async load(id) {
            if (!id.startsWith(prefix)) return null
            const [real, adapter] = id.slice(prefix.length).split('|')
            const [target, overrides] = await Promise.all([this.load({ id: real }), this.load({ id: adapter })])
            const own = new Set(overrides.exports)
            const stub = (name: string) => `stub(${JSON.stringify(`${name} from ${real}`)})`
            return [
                'const stub = name => new Proxy(function () {}, Object.fromEntries(["apply", "construct", "get"].map(trap => [trap, () => { throw new Error(`${name} reached by browser fixture`) }])))',
                ...target.exports.filter(name => /^[A-Za-z_$][\w$]*$/.test(name)).map(name => own.has(name)
                    ? `export { ${name} } from ${JSON.stringify(adapter)}`
                    : name === 'default' ? `export default ${stub(name)}` : `export const ${name} = ${stub(name)}`),
            ].join('\n')
        },
    }
}
