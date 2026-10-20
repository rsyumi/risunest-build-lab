import { describe, expect, it, vi } from 'vitest'
import { IDBFactory, IDBKeyRange } from 'fake-indexeddb'

vi.mock('./persistentDataRuntime.svelte', () => ({
    acquireDestructiveReplacementFence: vi.fn(),
    capturePersistentMutationToken: vi.fn(),
    getPersistentDataRuntime: vi.fn(),
}))

import {
    importNativeJpegAsset,
    type NativeJpegAssetImportDependencies,
} from './nativeJpegAssetImport'
import type { NativeFileJobActivationCommittedError } from './nativeFileJobs'
import type { Database } from './database.svelte'
import { IndexedDbPersistentDataStore } from './indexedDbPersistentDataStore'
import { capturePersistentPluginStorage, capturePersistentPresets, capturePersistentRoot, createPersistentDataRuntime } from './persistentDataRuntime'
import { registerCommittedWorkingSetContinuation, retryCommittedWorkingSetRefreshWithContinuation } from './committedWorkingSetContinuation'

function dependencies(statuses: unknown[]): NativeJpegAssetImportDependencies {
    const invoke = vi.fn(async (command: string) => {
        if (command === 'native_file_job_start') return { jobId: 'jpeg-job', warningCodes: [] }
        if (command === 'native_file_job_status') return statuses.shift()
        if (command === 'native_file_job_cancel') return { outcome: 'requested' }
        if (command === 'native_file_job_forget') return true
        throw new Error(`Unexpected command: ${command}`)
    })
    return {
        invoke,
        wait: vi.fn(async () => undefined),
        captureMutationToken: vi.fn(async () => ({ revision: 7, mutationGeneration: 3 })),
        acquireFence: vi.fn(async () => ({
            revision: 7,
            refreshCommittedWorkingSet: vi.fn(async () => undefined),
            release: vi.fn(),
        })),
        getRuntime: () => ({ markCommittedWorkingSetRefreshRequired: vi.fn(), getStorageAuthorityEpoch: () => 0 }),
    }
}

describe('native JPEG asset import', () => {
    it('refuses a changed portrait after fencing and before starting the native import', async () => {
        const deps = dependencies([])
        let fenced = false
        const acquire = deps.acquireFence
        deps.acquireFence = async token => { const fence = await acquire(token); fenced = true; return fence }
        const error = new Error('Synthetic stale portrait')
        await expect(importNativeJpegAsset({
            source: {type: 'desktopPath', path: 'C:\\chosen\\portrait.jpg'}, displayName: 'portrait.jpg',
            destination: {kind: 'current-character-image', characterId: 'synthetic-owner'},
        }, {beforeStart: async () => { expect(fenced).toBe(true); throw error }}, deps)).rejects.toBe(error)
        expect(deps.invoke).not.toHaveBeenCalled()
        const fence = await vi.mocked(acquire).mock.results[0].value
        expect(fence.release).toHaveBeenCalledWith({verifyStoreRevision: true})
    })

    it('sends an explicit current-character destination and refreshes the committed revision', async () => {
        const deps = dependencies([{
            jobId: 'jpeg-job',
            kind: 'import-jpeg-asset',
            state: 'succeeded',
            phase: 'complete',
            progress: {
                completedBytes: 4,
                totalBytes: 4,
                completedItems: 1,
                totalItems: 1,
            },
            result: {
                revision: 8,
                sourceBytes: 4,
                sourceSha256: 'a'.repeat(64),
                characterCount: 1,
                presetCount: 0,
                warningCodes: [],
            },
        }])

        const result = await importNativeJpegAsset({
            source: { type: 'desktopPath', path: 'C:\\chosen\\portrait.jpeg' },
            displayName: 'portrait.jpeg',
            destination: { kind: 'current-character-image', characterId: 'character-1' },
        }, {}, deps)

        expect(deps.invoke).toHaveBeenNthCalledWith(1, 'native_file_job_start', {
            request: {
                kind: 'import-jpeg-asset',
                source: { type: 'desktopPath', path: 'C:\\chosen\\portrait.jpeg' },
                displayName: 'portrait.jpeg',
                destination: { kind: 'current-character-image', characterId: 'character-1' },
                expectedRevision: 7,
            },
        })
        const fence = await vi.mocked(deps.acquireFence).mock.results[0].value
        expect(fence.refreshCommittedWorkingSet).toHaveBeenCalledWith(8)
        expect(fence.release).toHaveBeenCalledOnce()
        expect(result).toEqual({
            revision: 8,
            logicalId: `assets/${'a'.repeat(64)}.jpg`,
        })
    })

    it('cancels and drains without refreshing the prior character image', async () => {
        const deps = dependencies([
            {
                jobId: 'jpeg-job',
                kind: 'import-jpeg-asset',
                state: 'running',
                phase: 'reading-source',
                progress: { completedBytes: 1, completedItems: 0 },
            },
            {
                jobId: 'jpeg-job',
                kind: 'import-jpeg-asset',
                state: 'cancelled',
                phase: 'complete',
                progress: { completedBytes: 1, completedItems: 0 },
            },
        ])
        const controller = new AbortController()
        vi.mocked(deps.wait).mockImplementationOnce(async () => controller.abort())

        await expect(importNativeJpegAsset({
            source: { type: 'desktopPath', path: 'C:\\chosen\\portrait.jpg' },
            displayName: 'portrait.jpg',
            destination: { kind: 'current-character-image', characterId: 'character-1' },
        }, { signal: controller.signal }, deps)).rejects.toMatchObject({ name: 'AbortError' })

        expect(deps.invoke).toHaveBeenCalledWith('native_file_job_cancel', { jobId: 'jpeg-job' })
        const fence = await vi.mocked(deps.acquireFence).mock.results[0].value
        expect(fence.refreshCommittedWorkingSet).not.toHaveBeenCalled()
        expect(fence.release).toHaveBeenCalledOnce()
    })

    it('preserves the prior image when native activation reports a revision conflict', async () => {
        const deps = dependencies([{
            jobId: 'jpeg-job',
            kind: 'import-jpeg-asset',
            state: 'failed',
            phase: 'complete',
            progress: { completedBytes: 4, completedItems: 0 },
            error: { code: 'revision-conflict', message: 'revision changed' },
        }])

        await expect(importNativeJpegAsset({
            source: { type: 'androidSpool', token: 'spool-token' },
            displayName: 'portrait.jpeg',
            destination: { kind: 'current-character-image', characterId: 'character-1' },
        }, {}, deps)).rejects.toMatchObject({ code: 'revision-conflict' })

        const fence = await vi.mocked(deps.acquireFence).mock.results[0].value
        expect(fence.refreshCommittedWorkingSet).not.toHaveBeenCalled()
        expect(fence.release).toHaveBeenCalledOnce()
    })

    it('retains committed success for recovery when refreshing the renderer fails', async () => {
        const deps = dependencies([{
            jobId: 'jpeg-job',
            kind: 'import-jpeg-asset',
            state: 'succeeded',
            phase: 'complete',
            progress: { completedBytes: 4, completedItems: 1 },
            result: {
                revision: 8,
                sourceBytes: 4,
                sourceSha256: 'b'.repeat(64),
                characterCount: 1,
                presetCount: 0,
                warningCodes: [],
            },
        }])
        vi.mocked(deps.acquireFence).mockResolvedValueOnce({
            revision: 7,
            refreshCommittedWorkingSet: vi.fn(async () => {
                throw new Error('refresh unavailable')
            }),
            release: vi.fn(),
        })

        await expect(importNativeJpegAsset({
            source: { type: 'desktopPath', path: 'C:\\chosen\\portrait.jpg' },
            displayName: 'portrait.jpg',
            destination: { kind: 'current-character-image', characterId: 'character-1' },
        }, {}, deps)).rejects.toEqual(expect.objectContaining({
            name: 'NativeFileJobActivationCommittedError',
            code: 'activation-committed-refresh-failed',
            committedRevision: 8,
            recoveryRequired: true,
        } satisfies Partial<NativeFileJobActivationCommittedError>))

        expect(deps.invoke).not.toHaveBeenCalledWith('native_file_job_forget', {
            jobId: 'jpeg-job',
        })
        const acquiredFence = await vi.mocked(deps.acquireFence).mock.results[0].value
        expect(acquiredFence.release).toHaveBeenCalledOnce()
    })

    it.each(['success', 'lost-poll', 'no-commit', 'revision-read-failed', 'late-commit', 'unresolved-until-retry', 'continuation-in-flight'] as const)('reconciles the native revision before releasing the import fence (%s)', async (outcome) => {
        const initial = {
            username: 'User', botPresets: [], pluginCustomStorage: {},
            characters: [{ type: 'character', chaId: 'character-1', name: 'Character', chatPage: 0, chats: [] }],
        } as unknown as Database
        const store = new IndexedDbPersistentDataStore('jpeg-import-local-revision', new IDBFactory(), IDBKeyRange)
        await store.open()
        await store.replaceFromDatabase(structuredClone(initial))
        let database = structuredClone(initial)
        const onLocalRevision = vi.fn()
        const runtime = createPersistentDataRuntime({
            store,
            state: {
                captureRoot: () => capturePersistentRoot(database),
                capturePluginStorage: () => capturePersistentPluginStorage(database),
                capturePresets: () => capturePersistentPresets(database),
                captureSelectedCharacter: () => null,
                captureCharacter: (id) => database.characters.find((character) => character.chaId === id) ?? null,
                getSelectedCharacterId: () => undefined,
                replaceDatabase: (replacement) => { database = structuredClone(replacement) },
                publishCharacter: () => undefined,
                publishConversation: () => undefined,
            },
            prepareDatabase: async (value) => value,
            onLocalRevision,
        })
        await runtime.initializeActiveWorkingSet(database)
        const before = runtime.revision
        let committed = 0
        let statusReads = 0
        let allowCompletion = false
        let forgotten = false
        const nativeCommit = async () => {
            expect(() => runtime.assertPersistentMutationAllowed()).toThrow(/replacement is active/i)
            committed = (await store.commit({ expectedRevision: before,
                unitMutations: [{ type: 'set', key: JSON.stringify(['character', 'character-1', 'notes']), value: 'image' }],
            })).revision
        }
        const deps: NativeJpegAssetImportDependencies = {
            async invoke(command, args) {
                if (command === 'native_file_job_start') {
                    // The native job commits the new image at the fenced revision.
                    expect((args as { request: { expectedRevision: number } }).request.expectedRevision).toBe(before)
                    if (!['no-commit', 'late-commit', 'unresolved-until-retry', 'continuation-in-flight'].includes(outcome)) await nativeCommit()
                    return { jobId: 'jpeg-job' }
                }
                if (command === 'native_file_job_status') {
                    statusReads++
                    if (outcome === 'revision-read-failed') vi.spyOn(store, 'readRoot').mockRejectedValue(new Error('revision unavailable'))
                    if (outcome === 'late-commit' && statusReads > 1) await nativeCommit()
                    if (outcome !== 'success' && !(outcome === 'late-commit' && statusReads > 1) && !allowCompletion) throw new Error('status unavailable')
                    return {
                    jobId: 'jpeg-job', kind: 'import-jpeg-asset', state: 'succeeded', phase: 'complete',
                    progress: { completedBytes: 4, totalBytes: 4, completedItems: 1, totalItems: 1 },
                    result: { revision: committed, sourceBytes: 4, sourceSha256: 'b'.repeat(64), characterCount: 1, presetCount: 0, warningCodes: [] },
                    }
                }
                if (command === 'native_file_job_cancel') {
                    if (outcome === 'unresolved-until-retry' || outcome === 'continuation-in-flight') throw new Error('cancel unavailable')
                    return outcome === 'late-commit' ? 'tooLate' : outcome === 'no-commit' ? 'requested' : 'terminal'
                }
                if (command === 'native_file_job_forget') { forgotten = true; return true }
                throw new Error(`Unexpected command: ${command}`)
            },
            wait: async () => undefined,
            captureMutationToken: (reason) => runtime.capturePersistentMutationToken(reason),
            acquireFence: (token) => runtime.acquireDestructiveReplacementFence(token),
            getRuntime: () => runtime,
        }

        if (outcome === 'continuation-in-flight') {
            let entered!: () => void, finish!: () => void
            const running = new Promise<void>(resolve => { entered = resolve })
            const held = new Promise<void>(resolve => { finish = resolve })
            runtime.markCommittedWorkingSetRefreshRequired(before, new Error('earlier recovery'))
            registerCommittedWorkingSetContinuation(before, runtime, runtime.getStorageAuthorityEpoch(), async () => { entered(); await held }, undefined, true)
            const recovery = retryCommittedWorkingSetRefreshWithContinuation(runtime)
            await running
            expect(runtime.pendingWorkingSetRefreshRevision).toBeNull()
            const nativeCommands = vi.spyOn(deps, 'invoke')
            try {
                await expect(importNativeJpegAsset({
                    source: { type: 'desktopPath', path: 'synthetic-during-recovery.jpg' },
                    displayName: 'synthetic-during-recovery.jpg',
                    destination: { kind: 'current-character-image', characterId: 'character-1' },
                }, {}, deps)).rejects.toMatchObject({ name: 'PersistentMutationFencedError' })
                expect(nativeCommands).not.toHaveBeenCalled()
            } finally { finish(); await recovery }
            expect(() => runtime.assertPersistentMutationAllowed()).not.toThrow()
            return
        }

        const imported = importNativeJpegAsset({
            source: { type: 'desktopPath', path: 'C:\\chosen\\portrait.jpg' },
            displayName: 'portrait.jpg',
            destination: { kind: 'current-character-image', characterId: 'character-1' },
        }, {}, deps)
        if (outcome === 'success') await expect(imported).resolves.toMatchObject({ revision: before + 1 })
        else await expect(imported).rejects.toThrow('status unavailable')
        if (outcome === 'unresolved-until-retry') {
            expect(forgotten).toBe(false)
            expect(runtime.pendingWorkingSetRefreshRevision).toBe(before)
            expect(() => runtime.assertPersistentMutationAllowed()).toThrow(/replacement is active/i)
            const nativeCommands = vi.spyOn(deps, 'invoke')
            await expect(importNativeJpegAsset({
                source: { type: 'desktopPath', path: 'synthetic-second.jpg' },
                displayName: 'synthetic-second.jpg',
                destination: { kind: 'current-character-image', characterId: 'character-1' },
            }, {}, deps)).rejects.toThrow(/replacement is active/i)
            expect(nativeCommands).not.toHaveBeenCalled()
            await expect(runtime.retryCommittedWorkingSetRefresh()).rejects.toThrow(/replacement is active/i)
            await expect(retryCommittedWorkingSetRefreshWithContinuation(runtime)).rejects.toMatchObject({ code: 'native-job-outcome-unknown' })
            expect(() => runtime.assertPersistentMutationAllowed()).toThrow(/replacement is active/i)
            await nativeCommit()
            allowCompletion = true
            await expect(retryCommittedWorkingSetRefreshWithContinuation(runtime)).resolves.toMatchObject({ projection: 'applied', revision: before + 1 })
            expect(runtime.pendingWorkingSetRefreshRevision).toBeNull()
            expect(() => runtime.assertPersistentMutationAllowed()).not.toThrow()
            expect(onLocalRevision).toHaveBeenCalledExactlyOnceWith(before + 1)
            expect(forgotten).toBe(true)
            return
        }
        if (outcome === 'success' || outcome === 'lost-poll' || outcome === 'late-commit') expect(onLocalRevision).toHaveBeenCalledExactlyOnceWith(before + 1)
        else expect(onLocalRevision).not.toHaveBeenCalled()
        if (outcome === 'lost-poll' || outcome === 'revision-read-failed' || outcome === 'late-commit') {
            expect(runtime.pendingWorkingSetRefreshRevision).toBe(outcome === 'revision-read-failed' ? before : before + 1)
            expect(() => runtime.assertPersistentMutationAllowed()).toThrow(/replacement is active/i)
        } else {
            expect(runtime.pendingWorkingSetRefreshRevision).toBeNull()
            expect(() => runtime.assertPersistentMutationAllowed()).not.toThrow()
        }
    })
})
