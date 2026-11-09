import { beforeEach, describe, expect, it, vi } from 'vitest'
const mocks = vi.hoisted(() => ({ list: vi.fn(), invoke: vi.fn(), mutate: vi.fn(), owner: vi.fn() }))
vi.mock('../platform', async () => (await import('../platform.testSupport')).platformModule({ runtime: 'tauri', os: 'windows' }))
vi.mock('@tauri-apps/api/core', () => ({ invoke: mocks.invoke }))
vi.mock('./plugins.svelte', () => ({ pluginStorageStore: { forOwner: mocks.owner } }))
vi.mock('../storage/persistentDataStoreFactory', () => ({ getPersistentDataStore: () => ({ listPluginStorage: mocks.list }) }))
vi.mock('./pluginDeviceKeyspace', () => ({ invalidatePluginDeviceKeyspaces: vi.fn() }))
import { deletePluginDataForOwner, listPluginDataItems, pluginDataItemId, searchPluginDataValues } from './pluginDataInventory'
beforeEach(() => {
    vi.resetAllMocks()
    mocks.owner.mockReturnValue({ mutate: mocks.mutate, dispose: vi.fn() })
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
describe('device value listing', () => {
    it('lists a structure value by its type and leaves it out of value search', async () => {
        mocks.invoke.mockImplementation(async (command, args) => {
            if (command === 'pds_list_plugin_device_storage') return [
                { owner: 'plugin', space: 'json', key: 'bytes', byteSize: 9, structureType: 'Uint8Array' },
                { owner: 'plugin', space: 'json', key: 'text', byteSize: 9 },
            ]
            if (command === 'pds_read_plugin_device_value') return args.key === 'text' ? '"risunest"' : '#risunest-structured:1 {}'
        })
        const items = await listPluginDataItems('device')
        expect(items.map(item => [item.key, item.structureType, item.byteSize])).toEqual([['bytes', 'Uint8Array', 9], ['text', undefined, 9]])
        await expect(searchPluginDataValues(items, 'risunest', () => false)).resolves.toEqual(new Set([pluginDataItemId(items[1])]))
    })
})
