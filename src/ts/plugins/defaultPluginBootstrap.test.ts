import { flushSync } from 'svelte'
import { get, writable } from 'svelte/store'
import { afterEach, beforeEach, describe, expect, it, vi, type Mock } from 'vitest'
import { createNativeDeviceMarkers, DEVICE_MARKER_KEYS, type DeviceMarkerStorage } from '../storage/deviceMarkers'

const state = vi.hoisted(() => ({
    native: true, excluded: false, epoch: 0, syncBound: false, receive: vi.fn(), bindingRead: vi.fn(),
    db: { didFirstSetup: true, plugins: [] as Array<{ name: string, enabled?: boolean, version?: string }> },
    markers: null as unknown as DeviceMarkerStorage,
    importPlugin: vi.fn(), loadPlugins: vi.fn(), flush: vi.fn(), markDirty: vi.fn(), yield: vi.fn(),
}))
vi.mock('../platform', async () => (await import('../platform.testSupport')).platformModule(() => ({ runtime: state.native ? 'tauri' : 'web', os: 'windows' })))
vi.mock('../stores.svelte', () => ({ DBState: { get db() { return state.db } } }))
vi.mock('../storage/deviceMarkers', async original => ({
    ...await original<object>(), getDeviceMarkers: () => state.markers,
}))
vi.mock('../storage/deviceSettings', () => ({ getStartupExclusions: () => [] }))
vi.mock('../storage/recoveryMode.svelte', () => ({ isStartupExcluded: () => state.excluded }))
vi.mock('../../lib/Others/Onboarding/onboardingGate', () => ({ onboardingHold: writable(false) }))
vi.mock('../storage/persistentDataRuntime.svelte', () => ({
    persistentWorkingSetInputBlocked: writable(false),
    getPersistentStorageAuthorityEpoch: () => state.epoch,
    assertPersistentMutationAllowed: (epoch: number) => {
        if (epoch !== state.epoch || get(persistentWorkingSetInputBlocked)) throw new Error('replacement fenced')
    },
    flushPendingDataLocally: state.flush, markPersistentDataDirty: state.markDirty,
}))
vi.mock('./plugins.svelte', () => ({ importPlugin: state.importPlugin, loadPlugins: state.loadPlugins }))
vi.mock('../ui/yieldToUi', () => ({ yieldToUi: state.yield }))
vi.mock('../storage/sync/bindingNative', () => ({ createNativeSyncBindingBridge: () => ({
    state: async () => {
        await state.bindingRead()
        return { libraryId: 'synthetic', targetAuthority: '1', selectionEpoch: '1',
            target: state.syncBound ? { kind: 'server', connectionId: 'synthetic' } : { kind: 'none' } }
    },
    assertAuthority: async () => {},
}) }))
vi.mock('../storage/sync/bindingRegistry', async original => ({
    ...await original<typeof import('../storage/sync/bindingRegistry')>(),
    getSyncBindingTransport: () => ({ receiveAvailableChanges: state.receive }),
}))

import { onboardingHold } from '../../lib/Others/Onboarding/onboardingGate'
import { persistentWorkingSetInputBlocked } from '../storage/persistentDataRuntime.svelte'

const key = 'risuNestDefaultPluginsAttempted'
const names = ['provider-manager', 'yumi-translator']
let stored: Map<string, unknown>
let write: Mock<(name: string, value: unknown) => Promise<void>>
let fetcher: Mock<(url: string, options: RequestInit) => Promise<Response>>

function loadMarkers() {
    state.markers = createNativeDeviceMarkers({
        get: async name => stored.get(name) ?? null,
        readMany: async keys => keys.map(name => stored.get(name) ?? null),
        set: write, patch: vi.fn(),
    }, DEVICE_MARKER_KEYS.map(name => stored.get(name) ?? null))
}

async function start() {
    const module = await import('./defaultPluginBootstrap.svelte')
    module.startDefaultPluginBootstrap()
    flushSync()
    return module
}

beforeEach(() => {
    vi.resetModules()
    vi.clearAllMocks()
    state.native = true
    state.excluded = false
    state.epoch = 0
    state.syncBound = false
    state.bindingRead.mockResolvedValue(undefined)
    state.db = { didFirstSetup: true, plugins: [] }
    onboardingHold.set(false)
    // The production store is readonly; this mock owns the writable backing store.
    ;(persistentWorkingSetInputBlocked as ReturnType<typeof writable<boolean>>).set(false)
    stored = new Map()
    write = vi.fn(async (name: string, value: unknown) => { stored.set(name, value) })
    loadMarkers()
    state.yield.mockResolvedValue(undefined)
    state.flush.mockResolvedValue(undefined)
    state.loadPlugins.mockResolvedValue(undefined)
    state.importPlugin.mockImplementation(async (_code: string, options: { installIfMissing: string }) => {
        if (state.db.plugins.some(plugin => plugin.name === options.installIfMissing)) return false
        state.db.plugins.push({ name: options.installIfMissing })
        return true
    })
    fetcher = vi.fn(async () => new Response('synthetic plugin'))
    vi.stubGlobal('fetch', fetcher)
    vi.spyOn(console, 'error').mockImplementation(() => {})
})

afterEach(() => {
    vi.useRealTimers()
    vi.unstubAllGlobals()
    vi.restoreAllMocks()
})

describe('default plugins attempted once per ID', () => {
    it('rechecks a binding changed during its initial lookup without stranding defaults', async () => {
        state.syncBound = true
        let complete!: () => void
        state.bindingRead.mockImplementationOnce(() => new Promise<void>(resolve => { complete = resolve }))
        await start()
        await vi.waitFor(() => expect(state.bindingRead).toHaveBeenCalledOnce())
        state.syncBound = false
        ;(await import('../storage/sync/bindingChanges')).notifySyncBindingChanged()
        complete()
        await vi.waitFor(() => { flushSync(); expect(stored.get(key)).toBe(JSON.stringify(names)) })
        expect(state.receive).not.toHaveBeenCalled()
        await vi.waitFor(() => expect(state.db.plugins.map(plugin => plugin.name)).toEqual(names))
    })
    it('does not start a receive when all default attempts were already recorded', async () => {
        state.syncBound = true
        stored.set(key, JSON.stringify(names))
        loadMarkers()
        const read = vi.spyOn(state.markers, 'getItem')
        await start()
        await vi.waitFor(() => expect(read).toHaveBeenCalledWith(key))
        expect(state.receive).not.toHaveBeenCalled()
        expect(write).not.toHaveBeenCalled()
    })
    it('receives existing defaults before creating records even with no enabled plugins', async () => {
        state.syncBound = true
        let complete!: () => void
        state.receive.mockImplementationOnce(() => new Promise<void>(resolve => { complete = resolve }))
        await start()
        await vi.waitFor(() => expect(state.receive).toHaveBeenCalledOnce())
        expect(stored.has(key)).toBe(false)
        expect(fetcher).not.toHaveBeenCalled()
        state.db.plugins = names.map(name => ({ name, enabled: false, version: 'remote' }))
        complete()
        await vi.waitFor(() => { flushSync(); expect(stored.get(key)).toBe(JSON.stringify(names)) })
        expect(state.db.plugins).toEqual(names.map(name => ({ name, enabled: false, version: 'remote' })))
        expect(fetcher).not.toHaveBeenCalled()
        expect(state.importPlugin).not.toHaveBeenCalled()
    })
    it('waits for onboarding and restoration, records durably, then installs and reloads once', async () => {
        state.db.didFirstSetup = false
        onboardingHold.set(true)
        const blocked = persistentWorkingSetInputBlocked as ReturnType<typeof writable<boolean>>
        blocked.set(true)
        await start()
        expect(write).not.toHaveBeenCalled()
        state.db.didFirstSetup = true
        onboardingHold.set(false)
        flushSync()
        expect(write).not.toHaveBeenCalled()
        let commit!: () => void
        write.mockImplementationOnce(async (name: string, value: unknown) => {
            await new Promise<void>(resolve => { commit = resolve })
            stored.set(name, value)
        })
        blocked.set(false)
        flushSync()
        await vi.waitFor(() => expect(write).toHaveBeenCalledOnce())
        expect(fetcher).not.toHaveBeenCalled()
        commit()
        await vi.waitFor(() => expect(state.loadPlugins).toHaveBeenCalledOnce())
        expect(stored.get(key)).toBe(JSON.stringify(names))
        expect(fetcher.mock.calls.map(([url]) => url)).toEqual(names.map(name => `https://update.rsyumi.workers.dev/${name}.js`))
        expect(state.db.plugins.map(plugin => plugin.name)).toEqual(names)
        expect(state.flush).toHaveBeenCalledWith('default-plugin-install')
        expect(state.flush.mock.invocationCallOrder[0]).toBeLessThan(state.loadPlugins.mock.invocationCallOrder[0])
        expect(state.importPlugin.mock.calls.map(([, options]) => options)).toEqual(names.map(name => ({ installIfMissing: name, deferReload: true })))
    })

    it.each([[['provider-manager']], [names]])('skips existing records, including disabled older API records: %j', async existing => {
        state.db.plugins = existing.map(name => ({ name, enabled: false, version: '2.1' }))
        await start()
        await vi.waitFor(() => expect(stored.get(key)).toBe(JSON.stringify(names)))
        if (existing.length === 1) await vi.waitFor(() => expect(state.loadPlugins).toHaveBeenCalledOnce())
        expect(fetcher).toHaveBeenCalledTimes(2 - existing.length)
        expect(state.db.plugins.slice(0, existing.length)).toEqual(existing.map(name => ({ name, enabled: false, version: '2.1' })))
    })

    it('never retries after offline failure, another startup call, restart, or deletion', async () => {
        fetcher.mockRejectedValue(new Error('offline'))
        const module = await start()
        await vi.waitFor(() => expect(fetcher).toHaveBeenCalledTimes(2))
        module.startDefaultPluginBootstrap()
        vi.resetModules()
        loadMarkers()
        state.db.plugins = []
        await start()
        await state.markers.flush()
        expect(fetcher).toHaveBeenCalledTimes(2)
        expect(state.loadPlugins).not.toHaveBeenCalled()
        expect(write).toHaveBeenCalledOnce()
    })

    it('keeps the attempt when both plugins were present and one is later removed', async () => {
        state.db.plugins = names.map(name => ({ name }))
        await start()
        await vi.waitFor(() => expect(stored.get(key)).toBe(JSON.stringify(names)))
        state.db.plugins = []
        vi.resetModules()
        loadMarkers()
        await start()
        expect(fetcher).not.toHaveBeenCalled()
    })

    it('installs a newly added default on normal startup and retains previous IDs', async () => {
        const previous = ['retired-default', 'provider-manager']
        stored.set(key, JSON.stringify(previous))
        loadMarkers()
        const module = await start()
        await vi.waitFor(() => expect(state.loadPlugins).toHaveBeenCalledOnce())
        expect(fetcher.mock.calls.map(([url]) => url)).toEqual(['https://update.rsyumi.workers.dev/yumi-translator.js'])
        expect(state.db.plugins.map(plugin => plugin.name)).toEqual(['yumi-translator'])
        expect(stored.get(key)).toBe(JSON.stringify([...previous, 'yumi-translator']))

        // A later restore or sync does not start another pass in the running app.
        const blocked = persistentWorkingSetInputBlocked as ReturnType<typeof writable<boolean>>
        blocked.set(true)
        onboardingHold.set(true)
        flushSync()
        state.db.plugins = []
        blocked.set(false)
        onboardingHold.set(false)
        flushSync()
        module.startDefaultPluginBootstrap()
        vi.resetModules()
        loadMarkers()
        await start()
        await state.markers.flush()
        expect(fetcher).toHaveBeenCalledOnce()
        expect(write).toHaveBeenCalledOnce()
    })

    it('does not update a previously processed plugin when a new default is added', async () => {
        stored.set(key, JSON.stringify(['provider-manager']))
        loadMarkers()
        const existing = { name: 'provider-manager', enabled: false, version: '2.1' }
        state.db.plugins = [existing]
        await start()
        await vi.waitFor(() => expect(state.loadPlugins).toHaveBeenCalledOnce())
        expect(fetcher).toHaveBeenCalledOnce()
        expect(state.db.plugins).toEqual([existing, { name: 'yumi-translator' }])
    })

    it('does not retry a newly added default after an offline failure', async () => {
        stored.set(key, JSON.stringify(['provider-manager']))
        loadMarkers()
        fetcher.mockRejectedValue(new Error('offline'))
        await start()
        await vi.waitFor(() => expect(console.error).toHaveBeenCalled())
        expect(stored.get(key)).toBe(JSON.stringify(names))
        vi.resetModules()
        loadMarkers()
        await start()
        await state.markers.flush()
        expect(fetcher).toHaveBeenCalledOnce()
        expect(state.loadPlugins).not.toHaveBeenCalled()
    })

    it('continues to the other plugin after an HTTP failure', async () => {
        fetcher.mockResolvedValueOnce(new Response('unavailable', { status: 503 }))
        await start()
        await vi.waitFor(() => expect(state.loadPlugins).toHaveBeenCalledOnce())
        expect(state.db.plugins.map(plugin => plugin.name)).toEqual(['yumi-translator'])
    })

    it('does not pass an empty response to the file-picker importer', async () => {
        fetcher.mockResolvedValue(new Response('  '))
        await start()
        await vi.waitFor(() => expect(fetcher).toHaveBeenCalledTimes(2))
        expect(state.importPlugin).not.toHaveBeenCalled()
    })

    it('does not download when the attempt cannot be persisted', async () => {
        write.mockRejectedValueOnce(new Error('read only'))
        await start()
        await vi.waitFor(() => expect(console.error).toHaveBeenCalled())
        expect(fetcher).not.toHaveBeenCalled()
    })

    it('does not download when the attempt record cannot be read', async () => {
        const read = state.markers.getItem.bind(state.markers)
        vi.spyOn(state.markers, 'getItem').mockImplementation(name => {
            if (name === key) throw new Error('unreadable')
            return read(name)
        })
        await start()
        await vi.waitFor(() => expect(console.error).toHaveBeenCalled())
        expect(write).not.toHaveBeenCalled()
        expect(fetcher).not.toHaveBeenCalled()
    })

    it.each(['true', '{}', '[1]', 'invalid'])('does not overwrite an invalid attempt record: %s', async record => {
        stored.set(key, record)
        loadMarkers()
        await start()
        await vi.waitFor(() => expect(console.error).toHaveBeenCalled())
        expect(stored.get(key)).toBe(record)
        expect(write).not.toHaveBeenCalled()
        expect(fetcher).not.toHaveBeenCalled()
    })

    it('waits again if restoration starts before the UI yield finishes', async () => {
        let ready!: () => void
        state.yield.mockImplementationOnce(() => new Promise<void>(resolve => { ready = resolve }))
        await start()
        onboardingHold.set(true)
        flushSync()
        ready()
        await Promise.resolve()
        expect(write).not.toHaveBeenCalled()
        onboardingHold.set(false)
        flushSync()
        await vi.waitFor(() => expect(state.loadPlugins).toHaveBeenCalledOnce())
    })

    it('bounds hanging downloads and never retries the timed-out pass', async () => {
        vi.useFakeTimers()
        fetcher.mockImplementation((_url, options) => new Promise((_resolve, reject) => {
            options.signal!.addEventListener('abort', () => reject(new Error('timeout')), { once: true })
        }))
        const module = await start()
        await vi.waitFor(() => expect(fetcher).toHaveBeenCalledOnce())
        await vi.advanceTimersByTimeAsync(30_000)
        expect(fetcher).toHaveBeenCalledTimes(2)
        await vi.advanceTimersByTimeAsync(30_000)
        module.startDefaultPluginBootstrap()
        expect(fetcher).toHaveBeenCalledTimes(2)
        expect(state.loadPlugins).not.toHaveBeenCalled()
        expect(stored.get(key)).toBe(JSON.stringify(names))
    })

    it('allows a separate data directory to have its own first attempt', async () => {
        stored.set(key, JSON.stringify(names))
        loadMarkers()
        await start()
        expect(fetcher).not.toHaveBeenCalled()
        vi.resetModules()
        stored = new Map()
        loadMarkers()
        await start()
        await vi.waitFor(() => expect(state.loadPlugins).toHaveBeenCalledOnce())
        expect(fetcher).toHaveBeenCalledTimes(2)
    })

    it('does not apply downloads after a library replacement', async () => {
        fetcher.mockImplementationOnce(async () => {
            state.epoch += 1
            return new Response('stale download')
        })
        await start()
        await vi.waitFor(() => expect(console.error).toHaveBeenCalled())
        expect(state.importPlugin).not.toHaveBeenCalled()
        expect(state.loadPlugins).not.toHaveBeenCalled()
        expect(stored.get(key)).toBe(JSON.stringify(names))
    })

    it('preserves a duplicate installed while its download was pending', async () => {
        fetcher.mockImplementationOnce(async () => {
            state.db.plugins.push({ name: 'provider-manager', enabled: false })
            return new Response('downloaded')
        })
        await start()
        await vi.waitFor(() => expect(state.loadPlugins).toHaveBeenCalledOnce())
        expect(state.db.plugins).toEqual([{ name: 'provider-manager', enabled: false }, { name: 'yumi-translator' }])
    })

    it.each(['web', 'recovery'])('does not consume an attempt during %s startup', async mode => {
        state.native = mode !== 'web'
        state.excluded = mode === 'recovery'
        await start()
        expect(write).not.toHaveBeenCalled()
        expect(fetcher).not.toHaveBeenCalled()
    })
})
