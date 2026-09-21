import { readFileSync } from 'node:fs'
import { beforeEach, describe, expect, it, vi } from 'vitest'

const mocks = vi.hoisted(() => ({
    platform: 'web' as 'web' | 'desktop' | 'ios' | 'android',
    importIOS: vi.fn(), importAndroid: vi.fn(), importDesktop: vi.fn(),
    open: vi.fn(),
    readImage: vi.fn(),
    saveAsset: vi.fn(async (_data: Uint8Array) => ''),
}))

vi.mock('../platform', () => ({
    get isTauri() { return mocks.platform !== 'web' },
    get isTauriDesktop() { return mocks.platform === 'desktop' },
    get isTauriIOS() { return mocks.platform === 'ios' },
    get isTauriAndroid() { return mocks.platform === 'android' },
}))
vi.mock('../storage/iosContentPicker', () => ({ importIOSContentFromPicker: mocks.importIOS }))
vi.mock('../storage/androidContentPicker', () => ({ importAndroidContentFromPicker: mocks.importAndroid }))
vi.mock('../storage/nativeModuleFileRoute', () => ({ importDesktopNativeModulePath: mocks.importDesktop }))
vi.mock('@tauri-apps/plugin-dialog', () => ({ open: mocks.open }))

vi.mock('src/lang', () => ({
    language: {
        errors: { noData: 'no data' },
        successExport: 'exported',
        mcpStdioModuleImportBlocked: 'Local MCP module import blocked',
    },
}))
vi.mock('../alert', () => ({
    alertClear: vi.fn(),
    alertConfirm: vi.fn(),
    alertError: vi.fn(),
    alertModuleSelect: vi.fn(),
    alertNormal: vi.fn(),
    alertStore: { set: vi.fn() },
    alertWait: vi.fn(),
}))
vi.mock('../storage/database.svelte', () => ({
    getCurrentCharacter: vi.fn(),
    getCurrentChat: vi.fn(),
    getDatabase: vi.fn(),
    setCurrentCharacter: vi.fn(),
    setDatabase: vi.fn(),
}))
vi.mock('../globalApi.svelte', async () => {
    const { AppendableBuffer } = await import('../appendableBuffer')
    return {
        AppendableBuffer,
        downloadFile: vi.fn(),
        forageStorage: {},
        LocalWriter: class {},
        readImage: mocks.readImage,
        saveAsset: mocks.saveAsset,
        VirtualWriter: class {},
    }
})
vi.mock('../util', () => ({
    checkPersonaBinded: vi.fn(),
    selectSingleFile: vi.fn(),
    sleep: vi.fn(),
}))
vi.mock('uuid', () => ({ v4: () => 'roundtrip-module-id' }))
vi.mock('./lorebook.svelte', () => ({ convertExternalLorebook: vi.fn() }))
vi.mock('../stores.svelte', () => ({
    DBState: { db: { modules: [] } },
    HideIconStore: { set: vi.fn() },
    moduleBackgroundEmbedding: { set: vi.fn() },
    ReloadGUIPointer: { set: vi.fn() },
}))
vi.mock('../interchangeability', () => ({
    convertCharacterToModule: vi.fn(),
    convertModuleToCharacter: vi.fn(),
}))
vi.mock('../characterCards', () => ({
    exportCharacterCard: vi.fn(),
    importCharacterProcess: vi.fn(),
}))

import { exportModuleLegacy, importModule, importModuleData, readModule, type RisuModule } from './modules'
import { DBState } from '../stores.svelte'
import { alertConfirm, alertError, alertNormal } from '../alert'
import { StdioModuleImportError } from './mcp/moduleImport'

describe('legacy module export', () => {
    beforeEach(() => {
        vi.clearAllMocks()
        mocks.platform = 'web'
        DBState.db.modules = []
        const rpackMap = readFileSync('src/ts/rpack/rpack_map.bin')
        vi.stubGlobal('fetch', vi.fn(async () => ({
            arrayBuffer: async () => rpackMap.buffer.slice(
                rpackMap.byteOffset,
                rpackMap.byteOffset + rpackMap.byteLength,
            ),
        })))
    })

    it.each([false, true, undefined])('rejects JSON stdio modules regardless of lowLevelAccess=%s', async lowLevelAccess => {
        await importModuleData({
            name: 'synthetic.json',
            data: Buffer.from(JSON.stringify({
                type: 'risuModule', id: 'source', name: 'Synthetic', lowLevelAccess,
                mcp: { url: 'stdio:{"command":"node","args":["synthetic.js"]}' },
            })),
        })
        expect(DBState.db.modules).toEqual([])
        expect(alertConfirm).not.toHaveBeenCalled()
        expect(alertError).toHaveBeenCalledWith(expect.any(StdioModuleImportError))
        expect(alertNormal).not.toHaveBeenCalled()
    })

    it.each([false, true])('rejects legacy RISUM stdio before saving assets with lowLevelAccess=%s', async lowLevelAccess => {
        mocks.readImage.mockResolvedValue(new Uint8Array([1, 2, 3]))
        const exported = await exportModuleLegacy({
            id: 'source', name: 'Synthetic', description: '', lowLevelAccess,
            mcp: { url: 'stdio:{"command":"node","args":["synthetic.js"]}' },
            assets: [['synthetic', 'asset://synthetic', 'bin']],
        }, { alertEnd: false, saveData: false })

        await expect(readModule(Buffer.from(exported))).rejects.toBeInstanceOf(StdioModuleImportError)
        await importModuleData({ name: 'synthetic.risum', data: exported })
        expect(mocks.saveAsset).not.toHaveBeenCalled()
        expect(DBState.db.modules).toEqual([])
        expect(alertError).toHaveBeenCalledWith(expect.any(StdioModuleImportError))
    })

    it.each(['https://synthetic.invalid/mcp', 'internal:dice', 'plugin:synthetic'])('preserves %s in JSON and RISUM imports', async url => {
        const module = { id: 'source', name: 'Synthetic', description: '', mcp: { url } }
        await importModuleData({
            name: 'synthetic.json', data: Buffer.from(JSON.stringify({ ...module, type: 'risuModule' })),
        })
        const exported = await exportModuleLegacy(module, { alertEnd: false, saveData: false })
        await importModuleData({ name: 'synthetic.risum', data: exported })
        expect(DBState.db.modules.map(module => module.mcp?.url)).toEqual([url, url])
        expect(alertError).not.toHaveBeenCalled()
    })

    it.each(['ios', 'android', 'desktop'] as const)('routes the public module picker only to %s', async platform => {
        mocks.platform = platform
        mocks.open.mockResolvedValue('C:/synthetic/module.risum')
        mocks.importIOS.mockResolvedValue('ios-module')
        mocks.importAndroid.mockResolvedValue('android-module')
        await importModule()
        expect(mocks.importIOS).toHaveBeenCalledTimes(platform === 'ios' ? 1 : 0)
        expect(mocks.importAndroid).toHaveBeenCalledTimes(platform === 'android' ? 1 : 0)
        expect(mocks.importDesktop).toHaveBeenCalledTimes(platform === 'desktop' ? 1 : 0)
        if (platform === 'ios') expect(mocks.importIOS).toHaveBeenCalledWith('module')
        if (platform === 'android') expect(mocks.importAndroid).toHaveBeenCalledWith('module')
    })

    it('roundtrips ordinary asset bytes and preserves asset metadata exactly', async () => {
        const firstBytes = new Uint8Array([0x00, 0xff, 0x13, 0x7a, 0x80, 0x42])
        const secondBytes = new Uint8Array([0x91, 0x04, 0xcc, 0x2d, 0x7f])
        mocks.readImage.mockImplementation(async (source: string) => {
            if (source === 'asset://source-a') return firstBytes
            if (source === 'asset://source-b') return secondBytes
            throw new Error(`Unexpected asset source: ${source}`)
        })
        mocks.saveAsset
            .mockResolvedValueOnce('asset://roundtrip-a')
            .mockResolvedValueOnce('asset://roundtrip-b')
        const module: RisuModule = {
            id: 'source-module-id',
            name: 'Byte exact module',
            description: 'Legacy asset roundtrip',
            assets: [
                ['ordinary-asset-a', 'asset://source-a', 'bin', 'first-tail', { rank: 1 }],
                ['ordinary-asset-b', 'asset://source-b', 'dat', 'second-tail', { rank: 2 }],
            ] as unknown as [string, string, string][],
        }

        const exported = await exportModuleLegacy(module, { alertEnd: false, saveData: false })
        const imported = await readModule(Buffer.from(exported))

        expect(mocks.readImage.mock.calls.map(([source]) => source)).toEqual([
            'asset://source-a',
            'asset://source-b',
        ])
        expect(mocks.saveAsset).toHaveBeenCalledTimes(2)
        expect(mocks.saveAsset.mock.calls.map(([data]) => Array.from(data))).toEqual([
            Array.from(firstBytes),
            Array.from(secondBytes),
        ])
        expect(imported.assets).toEqual([
            ['ordinary-asset-a', 'asset://roundtrip-a', 'bin', 'first-tail', { rank: 1 }],
            ['ordinary-asset-b', 'asset://roundtrip-b', 'dat', 'second-tail', { rank: 2 }],
        ])
    })
})
