import { beforeEach, describe, expect, it, vi } from 'vitest'

import golden from './tests/fixtures/datasetExportGolden.json'

const mocks = vi.hoisted(() => ({
    materialize: vi.fn(),
    downloadFile: vi.fn(),
    alertNormal: vi.fn(),
    alertError: vi.fn(),
    exportNativeDataset: vi.fn(),
}))

vi.mock('./database.svelte', () => ({
    getDatabase: () => ({
        characters: [{
            type: 'character',
            chaId: 'catalog-only',
            name: 'Catalog only',
            chats: [],
        }],
    }),
}))
vi.mock('./persistentDataRuntime.svelte', () => ({
    materializePersistentDatabaseSnapshot: mocks.materialize,
}))
vi.mock('./nativeDatasetExportRoute', () => ({
    DATASET_EXPORT_FILE_NAME: 'dataset.json',
    exportNativeDataset: mocks.exportNativeDataset,
}))
vi.mock('../globalApi.svelte', () => ({ downloadFile: mocks.downloadFile }))
vi.mock('../alert', () => ({ alertNormal: mocks.alertNormal, alertError: mocks.alertError }))
vi.mock('src/lang', () => ({ language: { successExport: 'exported' } }))

function exportedDataset() {
    expect(mocks.downloadFile).toHaveBeenCalledOnce()
    const [name, bytes] = mocks.downloadFile.mock.calls[0] as [string, Uint8Array]
    expect(name).toBe('dataset.json')
    return JSON.parse(Buffer.from(bytes).toString('utf8'))
}

describe('exportAsDataset', () => {
    beforeEach(() => {
        mocks.materialize.mockReset().mockResolvedValue({
            characters: [
                {
                    type: 'character',
                    chaId: 'complete-character',
                    name: 'Complete character',
                    desc: 'Description',
                    globalLore: [{ key: 'lore' }],
                    chats: [{ id: 'chat', message: [{ role: 'user', data: 'hello' }] }],
                },
                {
                    type: 'group',
                    chaId: 'group',
                    name: 'Skipped group',
                    chats: [{ id: 'group-chat', message: [] }],
                },
            ],
        })
        mocks.downloadFile.mockReset().mockResolvedValue(undefined)
        mocks.alertNormal.mockReset()
        mocks.alertError.mockReset()
        mocks.exportNativeDataset.mockReset().mockResolvedValue(undefined)
    })

    it('exports every conversation from an authoritative detached snapshot', async () => {
        const { exportAsDataset } = await import('./exportAsDataset')

        await exportAsDataset()

        expect(mocks.materialize).toHaveBeenCalledWith('dataset-export')
        expect(exportedDataset()).toEqual([{
            name: 'Complete character',
            description: 'Description',
            chats: [{ role: 'user', data: 'hello' }],
            lorebook: [{ key: 'lore' }],
        }])
        expect(mocks.alertNormal).toHaveBeenCalledWith('exported')
    })

    it('matches the dataset the native export parses to on the shared fixture', async () => {
        mocks.materialize.mockResolvedValue({ characters: golden.characters })
        const { exportAsDataset } = await import('./exportAsDataset')

        await exportAsDataset()

        expect(exportedDataset()).toEqual(golden.dataset)
    })

    it('uses the native export without materializing the library', async () => {
        mocks.exportNativeDataset.mockResolvedValue({ revision: 7 })
        const { exportAsDataset } = await import('./exportAsDataset')

        await exportAsDataset()

        expect(mocks.materialize).not.toHaveBeenCalled()
        expect(mocks.downloadFile).not.toHaveBeenCalled()
        expect(mocks.alertNormal).toHaveBeenCalledWith('exported')
    })

    it('stays silent when the export picker is cancelled', async () => {
        mocks.exportNativeDataset.mockRejectedValue(
            new DOMException('Android SAF export was cancelled', 'AbortError'),
        )
        const { exportAsDataset } = await import('./exportAsDataset')

        await exportAsDataset()

        expect(mocks.alertNormal).not.toHaveBeenCalled()
        expect(mocks.alertError).not.toHaveBeenCalled()
        expect(mocks.materialize).not.toHaveBeenCalled()
    })

    it('reports a failed native export without falling back', async () => {
        const failure = new Error('disk full')
        mocks.exportNativeDataset.mockRejectedValue(failure)
        const { exportAsDataset } = await import('./exportAsDataset')

        await exportAsDataset()

        expect(mocks.alertError).toHaveBeenCalledWith(failure)
        expect(mocks.alertNormal).not.toHaveBeenCalled()
        expect(mocks.materialize).not.toHaveBeenCalled()
    })
})
