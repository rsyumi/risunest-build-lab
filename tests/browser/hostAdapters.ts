export const fixtureState = {
    database: { plugins: [] as any[] },
    writes: [] as { key: string; value: unknown }[],
    releaseHeld: null as null | (() => void),
    callbacks: new Set<(...args: any[]) => Promise<unknown>>(),
}
export const getV2PluginAPIs = () => ({})
export const pluginStorageStore = { forOwner: () => ({
    setItem: async (key: string, value: unknown) => { fixtureState.writes.push({ key, value }) },
    getItem: async (key: string) => key === 'held'
        ? new Promise(resolve => { fixtureState.releaseHeld = () => resolve('late-old-value') })
        : 'current-value',
}) }
export class SafeLocalStorage { constructor(_owner: string) {} }
export const beginPluginClaimSession = async () => null
export const registerTTSPreprocessor = (callback: (...args: any[]) => Promise<unknown>) => fixtureState.callbacks.add(callback)
export const unregisterTTSPreprocessor = (callback: (...args: any[]) => Promise<unknown>) => fixtureState.callbacks.delete(callback)
// Plugin load hashes the script, but only a permission check reads the hash, and no spec reaches one.
export const hasher = async () => 'synthetic-plugin-hash'
export const createProductionPluginDatabaseAccess = () => new Proxy({
    closeReadBaselines: () => {},
    expireReadBaselines: () => {},
}, { get: (target, key) => Reflect.get(target, key) ?? (() => { throw new Error(`Plugin database ${String(key)} reached by browser fixture`) }) })
