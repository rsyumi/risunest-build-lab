import { beforeEach, describe, expect, it, vi } from 'vitest'
const mocks = vi.hoisted(() => ({ list: vi.fn(), invoke: vi.fn(), mutate: vi.fn(), owner: vi.fn() }))
vi.mock('../platform', () => ({ isTauri: true }))
vi.mock('@tauri-apps/api/core', () => ({ invoke: mocks.invoke }))
vi.mock('./plugins.svelte', () => ({ pluginStorageStore: { forOwner: mocks.owner } }))
vi.mock('../storage/persistentDataStoreFactory', () => ({ getPersistentDataStore: () => ({ listPluginStorage: mocks.list }) }))
vi.mock('./pluginDeviceKeyspace', () => ({ invalidatePluginDeviceKeyspaces: vi.fn() }))
import { deletePluginDataForOwner } from './pluginDataInventory'
beforeEach(() => {
    vi.resetAllMocks()
    mocks.owner.mockReturnValue({ mutate: mocks.mutate })
    mocks.list.mockResolvedValue([
        { owner: 'plugin', key: 'ordinary', valueType: 'json', byteSize: 1 },
        { owner: 'other', key: 'ordinary', valueType: 'json', byteSize: 1 },
    ])
    mocks.invoke.mockImplementation(async command => command === 'pds_list_plugin_device_storage' ? [
        { owner: 'plugin', space: 'json', key: 'local-json', byteSize: 1 },
        { owner: 'plugin', space: 'string', key: 'local-string', byteSize: 1 },
        { owner: 'other', space: 'json', key: 'local-json', byteSize: 1 },
    ] : undefined)
})
describe('plugin removal data deletion', () => {
    it('deletes only the named owner in ordinary storage and both local namespaces', async () => {
        await deletePluginDataForOwner('plugin')
        expect(mocks.owner).toHaveBeenCalledExactlyOnceWith('plugin')
        expect(mocks.mutate).toHaveBeenCalledExactlyOnceWith([{ type: 'delete', key: 'ordinary' }])
        expect(mocks.invoke).toHaveBeenLastCalledWith('pds_write_plugin_device_values', { owner: 'plugin', mutations: [
            { type: 'delete', space: 'json', key: 'local-json' }, { type: 'delete', space: 'string', key: 'local-string' },
        ] })
    })
    it('reports ordinary deletion failure without continuing local deletion', async () => {
        mocks.mutate.mockRejectedValue(new Error('ordinary deletion failed'))
        await expect(deletePluginDataForOwner('plugin')).rejects.toThrow('ordinary deletion failed')
        expect(mocks.invoke).toHaveBeenCalledExactlyOnceWith('pds_list_plugin_device_storage')
    })
    it('reports local deletion failure', async () => {
        mocks.invoke.mockImplementation(async command => {
            if (command === 'pds_list_plugin_device_storage') return [{ owner: 'plugin', space: 'json', key: 'key', byteSize: 1 }]
            throw new Error('local deletion failed')
        })
        await expect(deletePluginDataForOwner('plugin')).rejects.toThrow('local deletion failed')
    })
    it('does not write when an owner has no stored values', async () => {
        await deletePluginDataForOwner('absent')
        expect(mocks.mutate).not.toHaveBeenCalled()
        expect(mocks.invoke).toHaveBeenCalledExactlyOnceWith('pds_list_plugin_device_storage')
    })
})
