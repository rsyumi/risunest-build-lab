import { describe, expect, it, vi } from 'vitest'

import {
    createCoordinatorOwnedAssetBlobStore,
    createRuntimeAssetRepositoryDispatcher,
} from './assetRepositoryRuntime'
import type { CompleteAssetRepositoryBlobStore } from './assetRepository'
import { PersistentMutationFencedError } from './saveCoordinator'

const metadata = {
    kind: 'asset' as const,
    mime: 'application/octet-stream',
    name: 'item.bin',
    ext: 'bin',
}

function repository(input: {
    prepareOwnedPut?: CompleteAssetRepositoryBlobStore['prepareOwnedPut']
} = {}): CompleteAssetRepositoryBlobStore {
    return {
        prepareOwnedPut: input.prepareOwnedPut ?? vi.fn(),
        prepareOwnedNewInlayImage: vi.fn(),
        put: vi.fn(),
        putNewInlayImage: vi.fn(),
        read: vi.fn(),
        stat: vi.fn(),
        list: vi.fn(),
        remove: vi.fn(),
        resolveUrl: vi.fn(),
    }
}

describe('current asset repository dispatcher', () => {
    it('prepares and activates writes through the one native repository', async () => {
        const abort = vi.fn(async () => undefined)
        const activate = vi.fn(async () => ({
            ...metadata,
            key: 'assets/item.bin',
            size: 1,
        }))
        const current = repository({
            prepareOwnedPut: vi.fn(async () => ({ abort, activate })),
        })
        const dispatcher = createRuntimeAssetRepositoryDispatcher(current)

        const staged = await dispatcher.stagePut(
            'assets/item.bin',
            Uint8Array.of(1),
            metadata,
        )

        await expect(dispatcher.activateStagedWrite(staged)).resolves.toEqual(
            expect.objectContaining({ key: 'assets/item.bin' }),
        )
        expect(activate).toHaveBeenCalledOnce()
        expect(abort).not.toHaveBeenCalled()
    })
})

describe('coordinated staged asset activation', () => {
    function harness() {
        let revision = 1
        let authorityEpoch = 1
        let failure: 'admission' | 'fence' | 'gate' | 'root' | null = null
        let prepareHook: (() => Promise<void>) | null = null
        let resolvePreparationStarted!: () => void
        const preparationStarted = new Promise<void>((resolve) => {
            resolvePreparationStarted = resolve
        })
        const handles: Array<{
            abort: ReturnType<typeof vi.fn>
            activate: ReturnType<typeof vi.fn>
        }> = []
        const current = repository({
            prepareOwnedPut: vi.fn(async () => {
                resolvePreparationStarted()
                await prepareHook?.()
                const handle = {
                    abort: vi.fn(async () => undefined),
                    activate: vi.fn(async () => {
                        revision++
                        return {
                            ...metadata,
                            key: 'assets/item.bin',
                            size: 1,
                        }
                    }),
                }
                handles.push(handle)
                return handle
            }),
        })
        const dispatcher = createRuntimeAssetRepositoryDispatcher(current)
        const runtime = {
            getStorageAuthorityEpoch: () => authorityEpoch,
            runStorageOnlyMutation(operation: (expectedRevision: number) => Promise<number>) {
                if (failure === 'admission' || failure === 'fence') {
                    const currentFailure = failure
                    failure = null
                    throw currentFailure === 'fence'
                        ? new PersistentMutationFencedError()
                        : new Error('coordinator admission failed')
                }
                return operation(revision).then((nextRevision) => {
                    revision = nextRevision
                })
            },
        }
        const authority = {
            rawStore: {
                async readRoot() {
                    if (failure === 'root') {
                        failure = null
                        throw new Error('root read failed')
                    }
                    return { revision, value: {} }
                },
            },
            gate: {
                async runKeyedWrite(_key: string, operation: () => Promise<unknown>) {
                    if (failure === 'gate') {
                        failure = null
                        throw new Error('keyed gate failed')
                    }
                    return operation()
                },
            },
        }
        return {
            handles,
            preparationStarted,
            currentRemove: current.remove as ReturnType<typeof vi.fn>,
            setFailure(value: typeof failure) {
                failure = value
            },
            setPrepareHook(value: (() => Promise<void>) | null) {
                prepareHook = value
            },
            bumpAuthorityEpoch() {
                authorityEpoch++
            },
            blob: createCoordinatorOwnedAssetBlobStore(dispatcher, authority as never, runtime),
        }
    }

    it.each([
        ['synchronous coordinator admission', 'admission'],
        ['destructive replacement fence', 'fence'],
        ['keyed gate admission', 'gate'],
        ['raw root precheck', 'root'],
    ] as const)('aborts exactly once after %s failure and settles the key tail', async (_name, point) => {
        const test = harness()
        test.setFailure(point)

        await expect(test.blob.put(
            'assets/item.bin',
            Uint8Array.of(1),
            metadata,
        )).rejects.toThrow()

        expect(test.handles[0].abort).toHaveBeenCalledOnce()
        expect(test.handles[0].activate).not.toHaveBeenCalled()
        await expect(test.blob.put(
            'assets/item.bin',
            Uint8Array.of(2),
            metadata,
        )).resolves.toEqual(expect.objectContaining({ key: 'assets/item.bin' }))
        expect(test.handles[1].activate).toHaveBeenCalledOnce()
    })

    it('aborts without alias activation when the storage authority changes', async () => {
        const test = harness()
        let releasePrepare!: () => void
        const prepareBlocked = new Promise<void>((resolve) => { releasePrepare = resolve })
        test.setPrepareHook(async () => prepareBlocked)

        const write = test.blob.put('assets/item.bin', Uint8Array.of(3), metadata)
        await test.preparationStarted
        test.bumpAuthorityEpoch()
        releasePrepare()

        await expect(write).rejects.toThrow('authority changed')
        expect(test.handles[0].abort).toHaveBeenCalledOnce()
        expect(test.handles[0].activate).not.toHaveBeenCalled()
    })

    it('rejects a queued delete after destructive replacement and settles the key tail', async () => {
        const test = harness()
        let releasePrepare!: () => void
        const prepareBlocked = new Promise<void>((resolve) => { releasePrepare = resolve })
        test.setPrepareHook(async () => prepareBlocked)

        const write = test.blob.put('assets/item.bin', Uint8Array.of(4), metadata)
        await test.preparationStarted
        const remove = test.blob.remove('assets/item.bin')
        const writeRejection = expect(write).rejects.toThrow('authority changed')
        const removeRejection = expect(remove).rejects.toThrow('authority changed')

        test.bumpAuthorityEpoch()
        releasePrepare()

        await writeRejection
        await removeRejection
        expect(test.currentRemove).not.toHaveBeenCalled()
        await expect(test.blob.remove('assets/item.bin')).resolves.toBeUndefined()
        expect(test.currentRemove).toHaveBeenCalledOnce()
    })
})
