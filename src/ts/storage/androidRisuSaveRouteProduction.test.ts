import { describe, expect, it, vi } from 'vitest'

const backupMocks = vi.hoisted(() => ({
    presented: vi.fn(),
    reason: vi.fn((code: string) => `localized:${code || "unknown"}`),
    replay: vi.fn(),
    restore: vi.fn(),
    externalManager: vi.fn(),
}))

vi.mock('../gui/nativeFileJobDialogModel', () => ({ failureReason: backupMocks.reason }))
vi.mock('./fileOperationErrorPresentation', () => ({ fileOperationErrorWasPresented: backupMocks.presented }))
vi.mock('./androidContentPicker', () => ({ importReplayedAndroidContentSpool: backupMocks.replay }))
vi.mock('src/lang', () => ({
    language: {
        risuSaveCleanupWarning: 'cleanup warning',
        risuSaveImportComplete: 'restore complete',
    },
}))
vi.mock('../alert', () => ({
    alertConfirm: vi.fn(),
    alertError: vi.fn(),
    alertNormal: vi.fn(),
}))
vi.mock('../platform', () => ({ isTauriAndroid: false }))
vi.mock('../plugins/plugins.svelte', () => ({
    loadPluginsAfterAuthoritativeRestore: vi.fn(),
}))
vi.mock('./persistentDataRuntime.svelte', () => ({
    getPersistentDataRuntime: vi.fn(),
}))
vi.mock('./portableBackupFileRouteProduction.svelte', () => ({
    restoreBackupFromNativeSource: backupMocks.restore,
}))
vi.mock('./nativeFileJobManager', () => ({
    runExternalAndroidNativeFileOperation: backupMocks.externalManager,
}))

import { alertConfirm, alertNormal, alertError } from '../alert'

import {
    showAndroidFileError,
    showDestinationRequired,
    importAndroidCharacterSpool,
    createAndroidOpenedSpoolDispatcher,
    dispatchAndroidOpenedSpoolBatch,
    importAndroidOpenedPreparedContent,
    restoreAndroidOpenedBackupSource,
    type AndroidOpenedSpoolDispatchDependencies,
} from './androidRisuSaveRouteProduction.svelte'
import type { PreparedNativeContent, PreparedNativeContentReceipt } from './nativeFileJobs'

describe('Android opened spool production route', () => {
    it.each(['backup.RISUNEST', 'compatible.BIN', 'block.RISUDAT'])(
        'delegates %s token and source metadata to the common restore owner without nesting operations',
        async (displayName) => {
            backupMocks.restore.mockReset()
            backupMocks.externalManager.mockClear()
            vi.mocked(alertConfirm).mockClear()
            vi.mocked(alertNormal).mockClear()
            const onSource = vi.fn()
            const source = {
                type: 'androidSpool' as const,
                token: '11111111-1111-4111-8111-111111111111',
            }
            backupMocks.restore.mockImplementation(async (factory) => {
                expect(
                    await factory({
                        signal: new AbortController().signal,
                        onStatus: vi.fn(),
                        onSource,
                    }),
                ).toEqual(source)
                return { warningCodes: [] }
            })

            await restoreAndroidOpenedBackupSource({ source, displayName })

            expect(backupMocks.restore).toHaveBeenCalledExactlyOnceWith(expect.any(Function))
            expect(onSource).toHaveBeenCalledExactlyOnceWith({ name: displayName })
            expect(backupMocks.externalManager).not.toHaveBeenCalled()
            expect(alertConfirm).not.toHaveBeenCalled()
            expect(alertNormal).toHaveBeenCalledExactlyOnceWith('restore complete')
        },
    )

    it('does not announce completion or discard source ownership after common restore cancellation', async () => {
        backupMocks.restore.mockReset().mockResolvedValue(null)
        vi.mocked(alertNormal).mockClear()
        await restoreAndroidOpenedBackupSource({
            source: { type: 'androidSpool', token: '11111111-1111-4111-8111-111111111111' },
            displayName: 'cancelled.risunest',
        })
        expect(alertNormal).not.toHaveBeenCalled()
    })

    it('preserves the committed cleanup warning from the common restore result', async () => {
        backupMocks.restore.mockReset().mockResolvedValue({ warningCodes: ['cleanup-failed'] })
        vi.mocked(alertNormal).mockClear()
        await restoreAndroidOpenedBackupSource({
            source: { type: 'androidSpool', token: '11111111-1111-4111-8111-111111111111' },
            displayName: 'restored.risunest',
        })
        expect(alertNormal).toHaveBeenCalledExactlyOnceWith('cleanup warning')
    })

    it('keeps the inactive content capability fallback out of the restore discard path', async () => {
        const enqueueRestore = vi.fn(async () => undefined)
        const importCharacter = vi.fn(async () => ({
            kind: 'capability-unavailable' as const,
        }))
        const dependencies: AndroidOpenedSpoolDispatchDependencies = {
            enqueueRestore,
            importCharacter,
            reportCharacterError: vi.fn(),
            reportDestinationRequired: vi.fn(),
        }
        const character = {
            token: '11111111-1111-4111-8111-111111111111',
            displayName: 'card.charx',
            bytes: 20,
        }
        const save = {
            token: '22222222-2222-4222-8222-222222222222',
            displayName: 'backup.risudat',
            bytes: 30,
        }
        const png = {
            token: '33333333-3333-4333-8333-333333333333',
            displayName: 'portrait.png',
            bytes: 40,
        }
        const risum = {
            token: '44444444-4444-4444-8444-444444444444',
            displayName: 'module.risum',
            bytes: 50,
        }

        await dispatchAndroidOpenedSpoolBatch({
            requestId: 'opened-1',
            ready: [character, save, png, risum],
            failures: [],
        }, dependencies)

        expect(importCharacter).toHaveBeenCalledTimes(3)
        expect(importCharacter).toHaveBeenNthCalledWith(1, character)
        expect(importCharacter).toHaveBeenNthCalledWith(2, png)
        expect(importCharacter).toHaveBeenNthCalledWith(3, risum)
        expect(enqueueRestore).toHaveBeenCalledExactlyOnceWith({
            requestId: 'opened-1',
            ready: [save],
            failures: [],
        })
        expect(dependencies.reportCharacterError).not.toHaveBeenCalled()
        expect(dependencies.reportDestinationRequired).not.toHaveBeenCalled()
    })

    it('sends every recognized Android content extension through the native character caller', async () => {
        const enqueueRestore = vi.fn(async () => undefined)
        const importCharacter = vi.fn(async () => ({ kind: 'declined' as const }))
        const dependencies: AndroidOpenedSpoolDispatchDependencies = {
            enqueueRestore,
            importCharacter,
            reportCharacterError: vi.fn(),
            reportDestinationRequired: vi.fn(),
        }
        const ready = [
            `${'a'.repeat(174)}.charx`,
            'card.json',
            'card.jpg',
            'card.JPEG',
            `${'p'.repeat(174)}.PnG`,
            `${'m'.repeat(174)}.RiSuM`,
            'book.LoReBoOk',
        ].map((displayName, index) => ({
            token: `00000000-0000-4000-8000-00000000000${index + 1}`,
            displayName,
            bytes: 10,
        }))

        await dispatchAndroidOpenedSpoolBatch({
            requestId: 'opened-2',
            ready,
            failures: [],
        }, dependencies)

        expect(importCharacter).toHaveBeenCalledTimes(7)
        ready.forEach((source, index) => {
            expect(importCharacter).toHaveBeenNthCalledWith(index + 1, source)
        })
        expect(enqueueRestore).toHaveBeenCalledExactlyOnceWith({
            requestId: 'opened-2',
            ready: [],
            failures: [],
        })
    })

    it('reports the explicit destination result for an ordinary Android JPEG', async () => {
        const dependencies: AndroidOpenedSpoolDispatchDependencies = {
            enqueueRestore: vi.fn(async () => undefined),
            importCharacter: vi.fn(async () => ({ kind: 'destination-required' as const })),
            reportCharacterError: vi.fn(),
            reportDestinationRequired: vi.fn(),
        }
        const source = {
            token: '11111111-1111-4111-8111-111111111111',
            displayName: 'portrait.jpeg',
            bytes: 10,
        }

        await dispatchAndroidOpenedSpoolBatch({
            requestId: 'opened-jpeg',
            ready: [source],
            failures: [],
        }, dependencies)

        expect(dependencies.reportDestinationRequired).toHaveBeenCalledExactlyOnceWith(source, expect.any(Number))
        expect(dependencies.reportCharacterError).not.toHaveBeenCalled()
    })

    it('deduplicates a replayed character token for one dispatcher lifetime', async () => {
        const dependencies: AndroidOpenedSpoolDispatchDependencies = {
            enqueueRestore: vi.fn(async () => undefined),
            importCharacter: vi.fn(async () => ({ kind: 'declined' as const })),
            reportCharacterError: vi.fn(),
            reportDestinationRequired: vi.fn(),
        }
        const dispatcher = createAndroidOpenedSpoolDispatcher(dependencies)
        const source = {
            token: '11111111-1111-4111-8111-111111111111',
            displayName: 'module.RISUM',
            bytes: 10,
        }

        await dispatcher.enqueue({ requestId: 'opened-1', ready: [source], failures: [] })
        await dispatcher.enqueue({ requestId: 'replayed-1', ready: [source], failures: [] })

        expect(dependencies.importCharacter).toHaveBeenCalledExactlyOnceWith(source)
    })

    it('keeps distinct character tokens with the same name in FIFO order', async () => {
        let releaseFirst!: () => void
        const calls: string[] = []
        const dependencies: AndroidOpenedSpoolDispatchDependencies = {
            enqueueRestore: vi.fn(async () => undefined),
            importCharacter: vi.fn(async (source) => {
                calls.push(source.token)
                if (calls.length === 1) {
                    await new Promise<void>((resolve) => releaseFirst = resolve)
                }
                return { kind: 'declined' as const }
            }),
            reportCharacterError: vi.fn(),
            reportDestinationRequired: vi.fn(),
        }
        const dispatcher = createAndroidOpenedSpoolDispatcher(dependencies)
        const first = {
            token: '11111111-1111-4111-8111-111111111111',
            displayName: 'card.PNG',
            bytes: 10,
        }
        const second = {
            token: '22222222-2222-4222-8222-222222222222',
            displayName: 'card.PNG',
            bytes: 10,
        }

        const firstBatch = dispatcher.enqueue({
            requestId: 'opened-1',
            ready: [first],
            failures: [],
        })
        await vi.waitFor(() => expect(calls).toEqual([first.token]))
        const secondBatch = dispatcher.enqueue({
            requestId: 'opened-2',
            ready: [second],
            failures: [],
        })
        await Promise.resolve()
        expect(calls).toEqual([first.token])

        releaseFirst()
        await Promise.all([firstBatch, secondBatch])
        expect(calls).toEqual([first.token, second.token])
    })

    it.each([
        ['png-card', 'character'] as const,
        ['risu-module', 'module'] as const,
    ])('prepares one Android token once and activates %s as %s', async (format, expected) => {
        const cancel = vi.fn(async () => undefined)
        const confirmActivated = vi.fn(async () => undefined)
        const receipt = {
            content: {
                casSessionId: 'session-1',
                format,
                metadata: {},
                assets: [],
                ...(format === 'risu-module'
                    ? { ownerHead: { present: false, manifestHash: null, entryCount: 0 } }
                    : {}),
            } as PreparedNativeContent,
            cancel,
            confirmActivated,
        } as unknown as PreparedNativeContentReceipt
        const prepare = vi.fn(async () => receipt)
        const activateCharacter = vi.fn(async () => ({ characterId: 'character-1' }))
        const activateModule = vi.fn(async () => ({ moduleId: 'module-1' }))
        const source = {
            token: '11111111-1111-4111-8111-111111111111',
            displayName: format === 'risu-module' ? 'module.RISUM' : 'card.PNG',
            bytes: 10,
        }

        const result = await importAndroidOpenedPreparedContent(source, {
            prepare,
            activateCharacter,
            activateModule,
        })

        expect(prepare).toHaveBeenCalledExactlyOnceWith(
            { type: 'androidSpool', token: source.token },
            source.displayName,
            { onStatus: expect.any(Function) },
        )
        expect(activateCharacter).toHaveBeenCalledTimes(expected === 'character' ? 1 : 0)
        expect(activateModule).toHaveBeenCalledTimes(expected === 'module' ? 1 : 0)
        expect(result).toEqual({
            kind: 'imported',
            value: expected === 'character' ? 'character-1' : 'module-1',
        })
        expect(confirmActivated).toHaveBeenCalledOnce()
        expect(cancel).not.toHaveBeenCalled()
    })

    it.each([
        ['card.PNG', 'risu-module', 'module'] as const,
        ['module.RISUM', 'png-card', 'character'] as const,
    ])(
        'uses native %s classifier result %s to activate %s',
        async (displayName, format, expected) => {
            const cancel = vi.fn(async () => undefined)
            const confirmActivated = vi.fn(async () => undefined)
            const receipt = {
                content: {
                    casSessionId: 'session-crossed',
                    format,
                    metadata: {},
                    assets: [],
                    ...(format === 'risu-module'
                        ? { ownerHead: { present: false, manifestHash: null, entryCount: 0 } }
                        : {}),
                } as PreparedNativeContent,
                cancel,
                confirmActivated,
            } as unknown as PreparedNativeContentReceipt
            const prepare = vi.fn(async () => receipt)
            const activateCharacter = vi.fn(async () => ({ characterId: 'character-crossed' }))
            const activateModule = vi.fn(async () => ({ moduleId: 'module-crossed' }))
            const enqueueRestore = vi.fn(async () => undefined)
            const source = {
                token: '55555555-5555-4555-8555-555555555555',
                displayName,
                bytes: 10,
            }
            const importCharacter = vi.fn(async (openedSource) =>
                await importAndroidOpenedPreparedContent(openedSource, {
                    prepare,
                    activateCharacter,
                    activateModule,
                }))

            await dispatchAndroidOpenedSpoolBatch({
                requestId: 'opened-crossed-classifier',
                ready: [source],
                failures: [],
            }, {
                enqueueRestore,
                importCharacter,
                reportCharacterError: vi.fn(),
                reportDestinationRequired: vi.fn(),
            })

            expect(enqueueRestore).toHaveBeenCalledExactlyOnceWith({
                requestId: 'opened-crossed-classifier',
                ready: [],
                failures: [],
            })
            expect(importCharacter).toHaveBeenCalledExactlyOnceWith(source)
            expect(prepare).toHaveBeenCalledExactlyOnceWith(
                { type: 'androidSpool', token: source.token },
                displayName,
                { onStatus: expect.any(Function) },
            )
            expect(activateCharacter).toHaveBeenCalledTimes(expected === 'character' ? 1 : 0)
            expect(activateModule).toHaveBeenCalledTimes(expected === 'module' ? 1 : 0)
            expect(confirmActivated).toHaveBeenCalledOnce()
            expect(cancel).not.toHaveBeenCalled()
        },
    )

    it('cancels a prepared Android token when activation is declined', async () => {
        const cancel = vi.fn(async () => undefined)
        const confirmActivated = vi.fn(async () => undefined)
        const prepare = vi.fn(async () => ({
            content: {
                casSessionId: 'session-1',
                format: 'risu-module',
                metadata: {},
                assets: [],
                ownerHead: { present: false, manifestHash: null, entryCount: 0 },
            },
            cancel,
            confirmActivated,
        } as unknown as PreparedNativeContentReceipt))

        const result = await importAndroidOpenedPreparedContent({
            token: '11111111-1111-4111-8111-111111111111',
            displayName: 'module.risum',
            bytes: 10,
        }, {
            prepare,
            activateCharacter: vi.fn(async () => ({ characterId: 'unused' })),
            activateModule: vi.fn(async () => null),
        })

        expect(result).toEqual({ kind: 'declined' })
        expect(prepare).toHaveBeenCalledOnce()
        expect(cancel).toHaveBeenCalledOnce()
        expect(confirmActivated).not.toHaveBeenCalled()
    })
})

it.each([['synthetic.JSON', 'auto'], ['synthetic.lorebook', 'module']] as const)(
    'classifies external %s before a native job can claim its token', async (displayName, destination) => {
        const source = { token: 'synthetic-token', displayName, bytes: 100 }
        backupMocks.replay.mockReset().mockResolvedValue('imported-id')
        backupMocks.externalManager.mockClear()
        await expect(importAndroidCharacterSpool(source)).resolves.toEqual({ kind: 'imported', value: 'imported-id' })
        expect(backupMocks.replay).toHaveBeenCalledExactlyOnceWith(source, destination)
        expect(backupMocks.externalManager).not.toHaveBeenCalled()
    },
)

it('localizes external preparation failures without exposing exception diagnostics', () => {
    backupMocks.presented.mockReturnValue(false)
    vi.mocked(alertError).mockClear()
    showAndroidFileError(Object.assign(new Error('private diagnostic source-busy'), { code: 'source-busy' }))
    expect(alertError).toHaveBeenCalledExactlyOnceWith('localized:source-busy')
    vi.mocked(alertError).mockClear()
    showAndroidFileError(new Error('private diagnostic'))
    expect(alertError).toHaveBeenCalledExactlyOnceWith('localized:unknown')
})
it('does not duplicate a fresh managed dialog failure', () => {
    backupMocks.presented.mockReturnValue(true)
    vi.mocked(alertError).mockClear()
    showAndroidFileError(new Error('managed'), 123)
    expect(backupMocks.presented).toHaveBeenLastCalledWith('import', 123)
    expect(alertError).not.toHaveBeenCalled()
    backupMocks.presented.mockReturnValue(false)
})

it('keeps the managed plain-JPEG failure as the sole outcome', () => {
    const source = { token: 'synthetic-token', displayName: 'photo.jpg', bytes: 100 }
    backupMocks.presented.mockReturnValue(true)
    vi.mocked(alertError).mockClear()
    showDestinationRequired(source, 123)
    expect(backupMocks.presented).toHaveBeenLastCalledWith('import', 123)
    expect(alertError).not.toHaveBeenCalled()
    backupMocks.presented.mockReturnValue(false)
    showDestinationRequired(source, 456)
    expect(alertError).toHaveBeenCalledExactlyOnceWith('photo.jpg: localized:destination-required')
})
