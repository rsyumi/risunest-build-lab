import { beforeEach, describe, expect, it, vi } from 'vitest'
import type { PluginDeviceMutation } from './pluginDeviceKeyspace'

const native = vi.hoisted(() => ({ invoke: vi.fn(), values: new Map<string, string>() }))
vi.mock('../platform', () => ({ isTauri: true }))
vi.mock('@tauri-apps/api/core', () => ({ invoke: native.invoke }))
vi.mock('./plugins.svelte', () => ({ pluginStorageStore: { invalidate: vi.fn() } }))
vi.mock('../storage/persistentDataStoreFactory', () => ({ getPersistentDataStore: vi.fn() }))

import { SafeLocalPluginStorage } from './pluginSafeClass'
import { invalidatePluginDeviceKeyspaces } from './pluginDeviceKeyspace'
import { deletePluginDataItems } from './pluginDataInventory'

beforeEach(() => {
    native.values.clear()
    invalidatePluginDeviceKeyspaces()
    native.invoke.mockReset().mockImplementation(async (command: string, args: {
        owner: string; mutations?: PluginDeviceMutation[];
    }) => {
        if (command === 'pds_hydrate_plugin_device_storage') {
            return { complete: true, byteSize: 0, entries: [...native.values].map(([key, value]) =>
                ({ space: 'json', key, value })) }
        }
        if (command === 'pds_write_plugin_device_values') {
            for (const mutation of args.mutations ?? []) {
                if (mutation.type === 'set') native.values.set(mutation.key, mutation.value)
                else if (mutation.type === 'delete') native.values.delete(mutation.key)
                else native.values.clear()
            }
            return
        }
        throw new Error(`Unexpected native command ${command}`)
    })
})

describe('native JSON plugin values', () => {
    it('preserves ordinary JSON and rejects unsupported values before the native boundary', async () => {
        const storage = new SafeLocalPluginStorage('native-json')
        for (const value of [{ nested: true }, [1, null], null, 42, 'text']) {
            await storage.setItem('value', value)
            await expect(storage.getItem('value')).resolves.toEqual(value)
        }
        const cycle: { self?: unknown } = {}; cycle.self = cycle
        native.invoke.mockClear()
        for (const value of [undefined, () => {}, Symbol('unsupported'), cycle, 1n]) {
            await expect(storage.setItem('value', value)).rejects.toBeInstanceOf(TypeError)
            await expect(storage.getItem('value')).resolves.toBe('text')
        }
        expect(native.invoke).not.toHaveBeenCalled()
        expect(native.values.get('value')).toBe('"text"')
    })

    it('invalidates the native hydrated cache after inventory deletion', async () => {
        const held = new SafeLocalPluginStorage('native-json')
        await held.setItem('value', { kept: true })
        await deletePluginDataItems([{ owner: 'native-json', space: 'json', key: 'value',
            valueType: 'json', byteSize: 13, automatic: false }], 'device')
        for (const wrapper of [held, new SafeLocalPluginStorage('native-json')]) {
            await expect(wrapper.getItem('value')).resolves.toBeNull()
            await expect(wrapper.keys()).resolves.toEqual([])
        }
    })
})
