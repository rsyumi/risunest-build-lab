import { describe, expect, it, vi } from 'vitest'

vi.mock('@tauri-apps/plugin-dialog', () => ({ save: vi.fn() }))
vi.mock('../platform', async () => (await import('../platform.testSupport')).platformModule({ runtime: 'web', os: 'windows' }))
vi.mock('./persistentDataRuntime.svelte', () => ({ getPersistentDataRuntime: vi.fn() }))
vi.mock('./database.svelte', () => ({ getDatabase: vi.fn() }))

import { get } from 'svelte/store'
import { exportNativeModuleRisumFromPicker } from './nativeModuleRisumExportRoute'
import { registerLibraryFileOperationGate } from './libraryFileOperation'
import { nativeFileOperation } from './nativeFileJobManager'

describe('native RISUM export route', () => {
    it('pins metadata-only module changes before exporting the exact identity index', async () => {
        const selected = { id: 'duplicate', name: 'Selected' }
        const modules = [{ id: 'duplicate', name: 'Other' }, selected]
        const calls: unknown[] = []
        let revision = 12
        let flushCount = 0
        const runtime = {
            get revision() { return revision },
            flushPendingData: async (reason: string) => {
                calls.push(['flush', reason])
                flushCount += 1
                if (flushCount === 2) {
                    selected.name = 'Selected after flush'
                    revision = 13
                }
            },
        }
        await exportNativeModuleRisumFromPicker(selected as never, {}, {
            isDesktop: () => true,
            isAndroid: () => false,
            chooseDestination: async () => 'C:\\chosen\\Selected.risum',
            runtime: () => runtime,
            modules: () => modules as never,
            runExport: async (input) => {
                calls.push(['export', input])
                return {
                    revision: 13,
                    sourceBytes: 7,
                    sourceFingerprintKind: 'whole-file-sha256' as const, sourceSha256: 'a'.repeat(64),
                    characterCount: 0,
                    presetCount: 0,
                    warningCodes: [],
                }
            },
        })
        expect(calls).toEqual([
            ['flush', 'native-risum-export'],
            ['flush', 'native-risum-export'],
            ['export', {
                moduleIndex: 1,
                expectedRevision: 13,
                destination: { type: 'desktopPath', path: 'C:\\chosen\\Selected.risum' },
            }],
        ])
    })

    it('rejects when the selected module moves while pinning its identity index', async () => {
        const selected = { id: 'selected', name: 'Selected' }
        const other = { id: 'other', name: 'Other' }
        const modules = [other, selected]
        let flushCount = 0
        const runExport = vi.fn()
        await expect(exportNativeModuleRisumFromPicker(selected as never, {}, {
            isDesktop: () => true,
            isAndroid: () => false,
            chooseDestination: async () => 'C:\\chosen\\Selected.risum',
            runtime: () => ({
                revision: 12 + flushCount,
                flushPendingData: async () => {
                    flushCount += 1
                    if (flushCount === 2) modules.splice(0, 2, selected, other)
                },
            }),
            modules: () => modules as never,
            runExport,
        })).rejects.toThrow('identity changed while pinning')
        expect(runExport).not.toHaveBeenCalled()
    })

    it('waits for sync and backups to settle before opening the picker', async () => {
        const module = { id: 'selected', name: 'Selected' }
        const calls: string[] = []
        let settle!: () => void
        const unregister = registerLibraryFileOperationGate(() => new Promise<void>(resolve => { settle = resolve }))
        try {
            const exporting = exportNativeModuleRisumFromPicker(module as never, {}, {
                isDesktop: () => true,
                isAndroid: () => false,
                chooseDestination: async () => { calls.push('pick'); return 'C:\\chosen\\Selected.risum' },
                runtime: () => ({ revision: 1, flushPendingData: async () => undefined }),
                modules: () => [module] as never,
                runExport: async () => { calls.push('export'); return null as never },
            })
            await vi.waitFor(() => expect(get(nativeFileOperation)?.waitingForSync).toBe(true))
            expect(calls).toEqual([])
            settle()
            await exporting
        } finally { unregister() }
        expect(calls).toEqual(['pick', 'export'])
    })

    it('keeps Web on the legacy path and rejects a detached equal-ID object', async () => {
        const module = { id: 'duplicate', name: 'Selected' }
        const base = {
            isAndroid: () => false,
            chooseDestination: async () => 'unused',
            runtime: () => ({ revision: 1, flushPendingData: async () => undefined }),
            modules: () => [module] as never,
            runExport: async () => { throw new Error('must not export') },
        }
        await expect(exportNativeModuleRisumFromPicker(module as never, {}, {
            ...base,
            isDesktop: () => false,
        })).resolves.toBeUndefined()
        await expect(exportNativeModuleRisumFromPicker({ ...module } as never, {}, {
            ...base,
            isDesktop: () => true,
        })).rejects.toThrow('exact root module')
    })
})
