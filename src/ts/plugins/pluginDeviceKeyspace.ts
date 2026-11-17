import { observePluginRemoteChanges, requestPluginStorageReload, StalePluginStorageError } from './pluginStorageProtection'
import { invoke } from '@tauri-apps/api/core'
import { isTauri } from '../platform'
import { pluginDeviceStorage, pluginDevicePrefix } from './pluginDeviceStorage'
import { readNativePluginDeviceValue, writeNativePluginDeviceValues } from './pluginDeviceTransfer'

export type PluginDeviceSpace = 'string' | 'json'

export const PLUGIN_DEVICE_CACHE_BYTES = 4 * 1024 * 1024

// Conservative UTF-16 payload and per-entry allowance, not a storage limit.
function cacheEntryBytes(key: string, value: string, sequence = ''): number {
    return 128 + 2 * (key.length + value.length + sequence.length)
}

export interface PluginDeviceEntry {
    space: PluginDeviceSpace
    key: string
    value: string
    sequence?: string
}

export interface PluginDeviceHydration {
    complete: boolean
    byteSize: number
    entries: PluginDeviceEntry[]
}

export type PluginDeviceMutation =
    | { type: 'set'; space: PluginDeviceSpace; key: string; value: string }
    | { type: 'delete'; space: PluginDeviceSpace; key: string }
    | { type: 'clear'; space: PluginDeviceSpace }

export interface PluginDeviceObservation { space: PluginDeviceSpace; key: string; sequence: string }

export interface PluginDeviceBackend {
    sequence?(owner: string, space: PluginDeviceSpace, key: string): Promise<string>
    hydrate(owner: string): Promise<PluginDeviceHydration>
    read(owner: string, space: PluginDeviceSpace, key: string): Promise<string | null>
    keys(owner: string, space: PluginDeviceSpace): Promise<string[]>
    write(owner: string, mutations: readonly PluginDeviceMutation[], observations?: readonly PluginDeviceObservation[]): Promise<void | PluginDeviceObservation[]>
}

/**
 * The native store answers by owner and space, so the browser backend builds
 * the same address out of one opaque key. An owner or key holding the separator
 * still cannot reach another plugin's keyspace.
 */
function browserKey(owner: string, space: PluginDeviceSpace, key: string): string {
    return `${pluginDevicePrefix}${JSON.stringify([owner, space, key])}`
}

function browserKeyOf(stored: string, owner: string, space: PluginDeviceSpace): string | null {
    if (!stored.startsWith(pluginDevicePrefix)) return null
    try {
        const parsed: unknown = JSON.parse(stored.substring(pluginDevicePrefix.length))
        if (!Array.isArray(parsed) || parsed.length !== 3) return null
        const [storedOwner, storedSpace, key] = parsed as unknown[]
        if (storedOwner !== owner || storedSpace !== space) return null
        return typeof key === 'string' ? key : null
    } catch {
        return null
    }
}

/**
 * The web build keeps the browser stores. Values stay strings on both sides so
 * one keyspace serves either backend without a second shape.
 */
export function createBrowserPluginDeviceBackend(): PluginDeviceBackend {
    const localKeys = (owner: string): string[] => {
        const keys: string[] = []
        for (let index = 0; index < localStorage.length; index += 1) {
            const stored = localStorage.key(index)
            const key = stored === null ? null : browserKeyOf(stored, owner, 'string')
            if (key !== null) keys.push(key)
        }
        return keys
    }
    const forageKeys = async (owner: string): Promise<string[]> => {
        const keys: string[] = []
        await pluginDeviceStorage.iterate((_value, stored) => {
            const key = browserKeyOf(stored, owner, 'json')
            if (key !== null) keys.push(key)
        })
        return keys
    }
    const read = async (
        owner: string,
        space: PluginDeviceSpace,
        key: string,
    ): Promise<string | null> => {
        if (space === 'string') return localStorage.getItem(browserKey(owner, space, key))
        return await pluginDeviceStorage.getItem<string>(browserKey(owner, space, key))
    }
    return {
        async hydrate(owner: string): Promise<PluginDeviceHydration> {
            const entries: PluginDeviceEntry[] = []
            let byteSize = 0
            for (const space of ['json', 'string'] as const) {
                const keys = space === 'string' ? localKeys(owner) : await forageKeys(owner)
                for (const key of keys.sort()) {
                    const value = await read(owner, space, key)
                    if (typeof value !== 'string') continue
                    byteSize += cacheEntryBytes(key, value)
                    if (byteSize > PLUGIN_DEVICE_CACHE_BYTES) {
                        return { complete: false, byteSize, entries: [] }
                    }
                    entries.push({ space, key, value })
                }
            }
            return { complete: true, byteSize, entries }
        },
        read,
        async keys(owner: string, space: PluginDeviceSpace): Promise<string[]> {
            return space === 'string' ? localKeys(owner) : await forageKeys(owner)
        },
        async write(owner: string, mutations: readonly PluginDeviceMutation[], observations?: readonly PluginDeviceObservation[]): Promise<void | PluginDeviceObservation[]> {
            for (const mutation of mutations) {
                if (mutation.type === 'clear') {
                    const keys =
                        mutation.space === 'string'
                            ? localKeys(owner)
                            : await forageKeys(owner)
                    for (const key of keys) {
                        if (mutation.space === 'string') {
                            localStorage.removeItem(browserKey(owner, mutation.space, key))
                        } else {
                            await pluginDeviceStorage.removeItem(
                                browserKey(owner, mutation.space, key),
                            )
                        }
                    }
                    continue
                }
                const stored = browserKey(owner, mutation.space, mutation.key)
                if (mutation.type === 'delete') {
                    if (mutation.space === 'string') localStorage.removeItem(stored)
                    else await pluginDeviceStorage.removeItem(stored)
                    continue
                }
                if (mutation.space === 'string') localStorage.setItem(stored, mutation.value)
                else await pluginDeviceStorage.setItem(stored, mutation.value)
            }
        },
    }
}

/**
 * Each call waits for its own native transaction, so a resolved write is
 * already on disk and a later read of the same key cannot miss it.
 */
export function createNativePluginDeviceBackend(): PluginDeviceBackend {
    return {
        sequence: (owner, space, key) => invoke<string>('pds_plugin_device_sequence', { owner, space, key }),
        async hydrate(owner: string): Promise<PluginDeviceHydration> {
            return await invoke<PluginDeviceHydration>('pds_hydrate_plugin_device_storage', {
                owner,
            })
        },
        async read(
            owner: string,
            space: PluginDeviceSpace,
            key: string,
        ): Promise<string | null> {
            return await readNativePluginDeviceValue(owner, space, key)
        },
        async keys(owner: string, space: PluginDeviceSpace): Promise<string[]> {
            return await invoke<string[]>('pds_list_plugin_device_keys', { owner, space })
        },
        async write(owner: string, mutations: readonly PluginDeviceMutation[], observations?: readonly PluginDeviceObservation[]): Promise<void | PluginDeviceObservation[]> {
            return await writeNativePluginDeviceValues(owner, mutations, observations)
        },
    }
}

/**
 * One plugin's device keyspace. The whole keyspace is loaded once when it fits
 * under the native limit; a larger one is read key by key and keeps the values
 * it reads or writes within the cache budget, oldest first out. Writes go out
 * as single changes and the cache follows the commit, never the other way
 * round.
 */
const liveKeyspaces = new Set<PluginDeviceKeyspace>()

type CachedEntry = PluginDeviceEntry & { order: number }

export class PluginDeviceKeyspace {
    readonly #owner: string
    readonly #backend: PluginDeviceBackend
    #cache: Map<PluginDeviceSpace, Map<string, CachedEntry>> | null = null
    // Only a complete cache answers a missing key or a key list by itself.
    #cacheComplete = false
    #cacheOrder = 0
    #hydration: Promise<void> | null = null
    #cacheBytes = 0
    #generation = 0
    #writeSequence = 0
    #pendingWrites = 0
    readonly #observed = new Map<string, PluginDeviceObservation>()
    readonly #stopObserving: () => void

    constructor(owner: string, backend: PluginDeviceBackend) {
        this.#owner = owner
        this.#backend = backend
        liveKeyspaces.add(this)
        this.#stopObserving = observePluginRemoteChanges(keys => {
            let reload = false
            for (const unit of keys) {
                const [kind, owner, space, key] = JSON.parse(unit) as string[]
                if (kind !== 'plugin-local' || owner !== this.#owner) continue
                this.invalidate()
                reload ||= this.#observed.has(JSON.stringify([space, key]))
            }
            return reload
        })
    }

    dispose(): void { this.#stopObserving(); liveKeyspaces.delete(this) }
    belongsTo(owner: string): boolean { return this.#owner === owner }

    async #reloadIfStale(observations: readonly PluginDeviceObservation[]): Promise<void> {
        if (!this.#backend.sequence || !liveKeyspaces.has(this)) return
        try {
            for (const value of observations) {
                if (await this.#backend.sequence(this.#owner, value.space, value.key) !== value.sequence) {
                    requestPluginStorageReload()
                    return
                }
            }
        } catch { requestPluginStorageReload() }
    }

    async #hydrate(): Promise<void> {
        if (this.#hydration !== null) {
            await this.#hydration
            return
        }
        const generation = this.#generation
        const started = (async () => {
            const loaded = await this.#backend.hydrate(this.#owner)
            if (generation !== this.#generation) return
            this.#cache = new Map([['string', new Map()], ['json', new Map()]])
            this.#cacheBytes = 0
            this.#cacheComplete = loaded.complete
            for (const entry of loaded.entries) this.#remember(entry)
        })()
        this.#hydration = started
        try {
            await started
        } catch (error) {
            if (this.#hydration === started) this.#hydration = null
            throw error
        }
    }

    invalidate(): void {
        this.#generation += 1
        this.#cache = null
        this.#cacheBytes = 0
        this.#hydration = null
    }

    // A value that does not fit is left out, and values that make room for a
    // newer one leave oldest first; either way the cache is no longer complete.
    #remember(entry: PluginDeviceEntry): void {
        const cache = this.#cache?.get(entry.space)
        if (!cache) return
        this.#forget(entry.space, entry.key)
        const bytes = cacheEntryBytes(entry.key, entry.value, entry.sequence)
        if (bytes > PLUGIN_DEVICE_CACHE_BYTES) {
            this.#cacheComplete = false
            return
        }
        while (this.#cacheBytes + bytes > PLUGIN_DEVICE_CACHE_BYTES) {
            let oldest: CachedEntry | undefined
            for (const entries of this.#cache!.values()) {
                const first = entries.values().next().value
                if (first && (!oldest || first.order < oldest.order)) oldest = first
            }
            if (!oldest) break
            this.#forget(oldest.space, oldest.key)
            this.#cacheComplete = false
        }
        cache.set(entry.key, { ...entry, order: ++this.#cacheOrder })
        this.#cacheBytes += bytes
    }

    #forget(space: PluginDeviceSpace, key: string): void {
        const cache = this.#cache?.get(space)
        const previous = cache?.get(key)
        if (!previous) return
        cache!.delete(key)
        this.#cacheBytes -= cacheEntryBytes(key, previous.value, previous.sequence)
    }

    async #write(mutation: PluginDeviceMutation): Promise<void> {
        this.#pendingWrites += 1
        try {
            await this.#commit(mutation)
        } finally {
            this.#pendingWrites -= 1
        }
    }

    async #commit(mutation: PluginDeviceMutation): Promise<void> {
        const observationGeneration = this.#generation
        const observations = [...this.#observed.values()].filter(value => value.space === mutation.space && (mutation.type === 'clear' || value.key === mutation.key))
        await this.#hydrate()
        const generation = this.#generation
        const sequence = ++this.#writeSequence
        let committed: readonly PluginDeviceObservation[] = []
        try {
            const observed = await this.#backend.write(this.#owner, [mutation], observations)
            committed = observed || []
            for (const value of observed || []) this.#observed.set(JSON.stringify([value.space, value.key]), value)
            if (observationGeneration !== this.#generation) await this.#reloadIfStale(observed || [])
        } catch (error) {
            this.invalidate()
            if (JSON.stringify(error)?.includes('PLUGIN_STORAGE_STALE') || String(error).includes('PLUGIN_STORAGE_STALE')) throw new StalePluginStorageError()
            throw error
        }
        if (generation !== this.#generation || sequence !== this.#writeSequence) {
            // A newer hydration may have read before this write committed.
            this.invalidate()
            return
        }
        const cache = this.#cache?.get(mutation.space)
        if (!cache) return
        if (mutation.type === 'clear') {
            for (const [key, entry] of cache) this.#cacheBytes -= cacheEntryBytes(key, entry.value, entry.sequence)
            cache.clear()
            return
        }
        if (mutation.type === 'delete') {
            this.#forget(mutation.space, mutation.key)
            return
        }
        this.#remember({ space: mutation.space, key: mutation.key, value: mutation.value,
            sequence: committed.find(value => value.space === mutation.space && value.key === mutation.key)?.sequence })
    }

    // A read is kept only when no write of this keyspace ran around it, so an
    // answer read before a write commits never replaces the written value.
    #keepRead(entry: PluginDeviceEntry, generation: number, writeSequence: number, quiet: boolean): void {
        if (quiet && this.#pendingWrites === 0 && generation === this.#generation && writeSequence === this.#writeSequence) {
            this.#remember(entry)
        }
    }

    async getItem(space: PluginDeviceSpace, key: string): Promise<string | null> {
        await this.#hydrate()
        const cached = this.#cache?.get(space)
        const entry = cached?.get(key)
        if (this.#backend.sequence) {
            if (entry?.sequence !== undefined) {
                this.#observed.set(JSON.stringify([space, key]), { space, key, sequence: entry.sequence })
                return entry.value
            }
            const generation = this.#generation, writeSequence = this.#writeSequence, quiet = this.#pendingWrites === 0
            const sequence = await this.#backend.sequence(this.#owner, space, key)
            const observation = { space, key, sequence }
            this.#observed.set(JSON.stringify([space, key]), observation)
            const value = await this.#backend.read(this.#owner, space, key)
            if (generation !== this.#generation) await this.#reloadIfStale([observation])
            else if (value !== null) this.#keepRead({ space, key, value, sequence }, generation, writeSequence, quiet)
            return value
        }
        if (entry) return entry.value
        if (cached && this.#cacheComplete) return null
        const generation = this.#generation, writeSequence = this.#writeSequence, quiet = this.#pendingWrites === 0
        const value = await this.#backend.read(this.#owner, space, key)
        if (value !== null) this.#keepRead({ space, key, value }, generation, writeSequence, quiet)
        return value
    }

    async keys(space: PluginDeviceSpace): Promise<string[]> {
        await this.#hydrate()
        const cached = this.#cache?.get(space)
        const keys = cached && this.#cacheComplete ? [...cached.keys()] : await this.#backend.keys(this.#owner, space)
        return keys.sort()
    }

    async setItem(space: PluginDeviceSpace, key: string, value: string): Promise<void> {
        await this.#write({ type: 'set', space, key, value })
    }

    async removeItem(space: PluginDeviceSpace, key: string): Promise<void> {
        await this.#write({ type: 'delete', space, key })
    }

    async clear(space: PluginDeviceSpace): Promise<void> {
        await this.#write({ type: 'clear', space })
    }
}

let backend: PluginDeviceBackend | null = null
const keyspaces = new Map<string, PluginDeviceKeyspace>()

function activeBackend(): PluginDeviceBackend {
    backend ??= isTauri ? createNativePluginDeviceBackend() : createBrowserPluginDeviceBackend()
    return backend
}

export function createPluginDeviceKeyspace(owner: string): PluginDeviceKeyspace {
    return new PluginDeviceKeyspace(owner, activeBackend())
}

export function getPluginDeviceKeyspace(owner: string): PluginDeviceKeyspace {
    let keyspace = keyspaces.get(owner)
    if (!keyspace) {
        keyspace = new PluginDeviceKeyspace(owner, activeBackend())
        keyspaces.set(owner, keyspace)
    }
    return keyspace
}

/** Invalidates held wrappers too; plugin-owned copies remain outside this cache. */
export function invalidatePluginDeviceKeyspaces(owner?: string): void {
    for (const keyspace of liveKeyspaces) {
        if (owner === undefined || keyspace.belongsTo(owner)) keyspace.invalidate()
    }
}
