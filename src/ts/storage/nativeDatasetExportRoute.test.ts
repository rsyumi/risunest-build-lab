import { describe, expect, it, vi } from 'vitest'

vi.mock('@tauri-apps/api/path', () => ({
    downloadDir: vi.fn(async () => 'C:\\Users\\me\\Downloads'),
    join: vi.fn(async (...parts: string[]) => parts.join('\\')),
}))
vi.mock('../platform', () => ({ isTauriAndroid: false, isTauriDesktop: false, isTauriIOS: false }))
vi.mock('./persistentDataRuntime.svelte', () => ({
    getPersistentDataRuntime: vi.fn(),
}))

import { exportNativeDataset } from './nativeDatasetExportRoute'
import type { NativeFileJobResult } from './nativeFileJobs'
import { get } from 'svelte/store'
import { doingChat, reserveGeneration } from '../process/generationState'
import { isLibraryFileOperationReserved } from './libraryFileOperation'
import { dismissNativeFileOperationOutcome, nativeFileOperationOutcome, runSharedNativeFileOperation } from './nativeFileJobManager'

function dependencies(platform: 'desktop' | 'android' | 'ios' | 'web', calls: unknown[]) {
    let revision = 3
    return {
        isDesktop: () => platform === 'desktop',
        isAndroid: () => platform === 'android',
        isIOS: () => platform === 'ios',
        runtime: () => ({
            get revision() { return revision },
            flushPendingData: async (reason: string) => {
                calls.push(['flush', reason])
                revision = 4
            },
        }),
        desktopDestination: async (fileName: string) => {
            calls.push(['desktopDestination', fileName])
            return `C:\\Users\\me\\Downloads\\${fileName}`
        },
        runExport: async (input: unknown) => {
            calls.push(['runExport', input])
            return { revision: 4 } as NativeFileJobResult
        },
    }
}

describe('native dataset export route', () => {
    it('admits before flush and joins rapid clicks through destination publication', async () => {
        const calls: unknown[] = []
        const deps = dependencies('android', calls)
        let finish!: () => void
        const publication = new Promise<void>(resolve => { finish = resolve })
        let entered!: () => void
        const preparing = new Promise<void>(resolve => { entered = resolve })
        const runExport = vi.fn(async () => {
            expect(isLibraryFileOperationReserved()).toBe(true)
            expect(reserveGeneration()).toBeNull()
            entered()
            await publication
            return { revision: 4 } as NativeFileJobResult
        })
        dismissNativeFileOperationOutcome()
        const first = exportNativeDataset({}, { ...deps, runExport })
        const second = exportNativeDataset({}, { ...deps, runExport })
        await preparing
        expect(runExport).toHaveBeenCalledOnce()
        expect(calls).toEqual([['flush', 'native-dataset-export']])
        expect(get(nativeFileOperationOutcome)).toBeNull()
        await expect(runSharedNativeFileOperation('import', 'other', async () => undefined)).rejects.toMatchObject({ name: 'NativeFileOperationBusyError' })
        finish()
        expect(await first).toEqual(await second)
        expect(get(nativeFileOperationOutcome)).toMatchObject({ state: 'succeeded', format: 'dataset' })
        expect(isLibraryFileOperationReserved()).toBe(false)
    })

    it('rejects active generation before capturing or starting', async () => {
        const calls: unknown[] = []
        doingChat.set(true)
        try {
            await expect(exportNativeDataset({}, dependencies('android', calls))).rejects.toMatchObject({ code: 'generation-active' })
            expect(calls).toEqual([])
        } finally { doingChat.set(false) }
    })

    it('shows native publication failure once and never selects renderer fallback', async () => {
        const calls: unknown[] = []
        const deps = { ...dependencies('android', calls), runExport: async () => { throw new Error('synthetic destination failure') } }
        await expect(exportNativeDataset({}, deps)).resolves.toBeNull()
        expect(get(nativeFileOperationOutcome)).toMatchObject({ state: 'failed', format: 'dataset' })
        expect(isLibraryFileOperationReserved()).toBe(false)
    })
    it('writes the desktop dataset into Downloads after flushing pending edits', async () => {
        const calls: unknown[] = []

        await expect(exportNativeDataset({}, dependencies('desktop', calls))).resolves.toEqual({ revision: 4 })

        expect(calls).toEqual([
            ['desktopDestination', 'dataset.json'],
            ['flush', 'native-dataset-export'],
            ['runExport', {
                destination: { type: 'desktopPath', path: 'C:\\Users\\me\\Downloads\\dataset.json' },
                expectedRevision: 4,
            }],
        ])
    })

    it.each([
        ['android', 'androidSaf'],
        ['ios', 'iosFiles'],
    ] as const)('hands the %s dataset to the platform export picker', async (platform, type) => {
        const calls: unknown[] = []

        await exportNativeDataset({}, dependencies(platform, calls))

        expect(calls).toEqual([
            ['flush', 'native-dataset-export'],
            ['runExport', {
                destination: { type, suggestedName: 'dataset.json' },
                expectedRevision: 4,
            }],
        ])
    })

    it('leaves the web build to the renderer export', async () => {
        const calls: unknown[] = []

        await expect(exportNativeDataset({}, dependencies('web', calls))).resolves.toBeUndefined()

        expect(calls).toEqual([])
    })
})
