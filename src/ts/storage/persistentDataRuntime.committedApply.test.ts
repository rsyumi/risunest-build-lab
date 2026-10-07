import { applyRootMutations } from './rootMutation'
import { acquireUpstreamImportPause } from './upstreamReplacement'
import { describe, expect, it, vi } from 'vitest'
import type { Database } from './database.svelte'
import type { PersistentDataStore, PersistentRevisionLease } from './persistentDataStore'
import {
    capturePersistentPluginStorage,
    capturePersistentPresets,
    capturePersistentRoot,
    createPersistentDataRuntime,
} from './persistentDataRuntime'
import { PersistentMutationFencedError, type OfficialRevisionPublisher } from './saveCoordinator'
import { deferred, makeDatabase } from './saveCoordinator.testSupport'
import {
    registerCommittedWorkingSetContinuation,
    retryCommittedWorkingSetRefreshWithContinuation,
} from './committedWorkingSetContinuation'
import {
    hasPendingExternalApplication,
    retryExternalApplication,
    runExternalApplication,
    type ExternalApplicationConfirmation,
} from './sync/external/applicationRecovery'

async function createHarness(officialPublisher?: OfficialRevisionPublisher, captureWorkingSet = false) {
    let database = makeDatabase()
    let durable = structuredClone(database)
    let revision = 1
    let generating = false
    const leases: PersistentRevisionLease[] = []
    const replaceDatabase = vi.fn((replacement: Database) => { database = replacement })
    const onWorkingSetRefreshRequired = vi.fn()
    const onBackgroundError = vi.fn()
    const afterRemoteRootChange = vi.fn()
    const publishPresetWorkingSet = vi.fn()
    const store = {
        open: vi.fn(async () => undefined),
        readRoot: vi.fn(async () => ({ revision, value: capturePersistentRoot(durable) })),
        commit: vi.fn(async () => { throw new Error('Unexpected content commit') }),
        replaceFromDatabase: vi.fn(async (replacement: Database, expected: number) => {
            expect(expected).toBe(revision)
            durable = structuredClone(replacement)
            return { revision: ++revision }
        }),
        acquireRevision: vi.fn(async (expected: number) => {
            expect(expected).toBe(revision)
            const snapshot = structuredClone(durable)
            const lease = {
                revision: expected,
                readRoot: vi.fn(async () => ({ revision: expected, value: capturePersistentRoot(snapshot) })),
                queryPresets: vi.fn(async () => ({ revision: expected, items: [] })),
                readPreset: vi.fn(async () => null),
                queryCharacters: vi.fn(async ({ trash }: { trash: boolean }) => ({
                    revision: expected,
                    items: trash ? [] : snapshot.characters.map((character, configuredIndex) => ({
                        id: character.chaId,
                        name: character.name,
                        type: character.type,
                        configuredIndex,
                        recentAt: 0,
                        trashed: false,
                        conversationCount: 0,
                    })),
                })),
                readCharacterSummary: vi.fn(async (id: string) => {
                    const configuredIndex = snapshot.characters.findIndex((character) => character.chaId === id)
                    if (configuredIndex < 0) return null
                    const character = snapshot.characters[configuredIndex]
                    return {
                        id, name: character.name, type: character.type, configuredIndex,
                        recentAt: 0, trashed: false, conversationCount: 0,
                    }
                }),
                readCharacter: vi.fn(async (id: string) => {
                    const value = snapshot.characters.find((character) => character.chaId === id)
                    if (!value) return null
                    const { chats: _chats, ...detail } = value
                    return { revision: expected, value: detail }
                }),
                queryConversations: vi.fn(async () => ({ revision: expected, items: [] })),
                queryPluginStorage: vi.fn(async () => ({ revision: expected, items: [] })),
                release: vi.fn(async () => undefined),
            } as unknown as PersistentRevisionLease
            leases.push(lease)
            return lease
        }),
        materializeDatabase: vi.fn(async () => { throw new Error('Unexpected complete materialization') }),
    } as unknown as PersistentDataStore
    const runtime = createPersistentDataRuntime({
        store,
        state: {
            captureWorkingSetDatabase: captureWorkingSet ? () => database : undefined,
            captureRoot: () => capturePersistentRoot(database),
            capturePluginStorage: () => capturePersistentPluginStorage(database),
            capturePresets: () => capturePersistentPresets(database),
            captureSelectedCharacter: () => database.characters[0] ?? null,
            captureCharacter: (id) => database.characters.find((value) => value.chaId === id) ?? null,
            getSelectedCharacterId: () => database.characters[0]?.chaId,
            getSelectedConversationId: () => null,
            replaceDatabase,
            afterRemoteRootChange,
            publishCharacter: vi.fn(),
            publishConversation: vi.fn(),
            publishPresetWorkingSet,
            isConversationOperationActive: () => generating,
        },
        prepareDatabase: async (value) => structuredClone(value),
        officialPublisher,
        onWorkingSetRefreshRequired,
        onBackgroundError,
        clock: { setTimeout: vi.fn(() => 1), clearTimeout: vi.fn() },
    })
    await runtime.initializeActiveWorkingSet(database)
    return {
        runtime, store, replaceDatabase, publishPresetWorkingSet,
        onWorkingSetRefreshRequired, onBackgroundError, afterRemoteRootChange, leases,
        get database() { return database },
        get durable() { return durable },
        set generating(value: boolean) { generating = value },
        nativeCommit(replacement: Database) {
            durable = structuredClone(replacement)
            return ++revision
        },
    }
}

describe('received display settings after whole-library projection', () => {
    it.each(['refresh', 'activation'] as const)('reports only the successfully installed settings after %s', async (mode) => {
        const harness = await createHarness()
        const replacement = structuredClone(harness.database)
        replacement.animationSpeed = 0.4
        replacement.heightMode = 'dvh'
        replacement.sideBarSize = 2
        replacement.textAreaSize = 3
        replacement.textAreaTextSize = 4
        harness.replaceDatabase.mockImplementationOnce((projected) => {
            // Installation can normalize values; report the installed root instead of the projection.
            projected.animationSpeed = 0.5
            harness.replaceDatabase.getMockImplementation()!(projected)
        })
        harness.afterRemoteRootChange.mockImplementation((fields) => {
            expect(harness.database.animationSpeed).toBe(0.5)
            expect(fields).toEqual(new Set(['animationSpeed', 'heightMode', 'sideBarSize', 'textAreaSize', 'textAreaTextSize']))
        })
        if (mode === 'activation') {
            await harness.runtime.withPausedPersistentWrites('binding-activation', async (token) => {
                const guard = harness.runtime.beginActivatedLibraryGuard(token)
                harness.nativeCommit(replacement)
                await harness.runtime.refreshActivatedLibraryUnderPause(token)
                guard.complete()
            })
        } else {
            const token = await harness.runtime.capturePersistentMutationToken('restore')
            const fence = await harness.runtime.acquireDestructiveReplacementFence(token)
            await fence.refreshCommittedWorkingSet(harness.nativeCommit(replacement))
            fence.release()
        }
        expect(harness.afterRemoteRootChange).toHaveBeenCalledOnce()
        expect(harness.onBackgroundError).not.toHaveBeenCalled()
    })

    it('does not apply settings for an unchanged whole-library refresh', async () => {
        const harness = await createHarness()
        const token = await harness.runtime.capturePersistentMutationToken('restore')
        const fence = await harness.runtime.acquireDestructiveReplacementFence(token)
        await fence.refreshCommittedWorkingSet(harness.nativeCommit(harness.database))
        fence.release()
        expect(harness.afterRemoteRootChange).not.toHaveBeenCalled()
    })

    it.each(['refresh', 'activation'] as const)('does not apply settings when %s installation fails', async (mode) => {
        const harness = await createHarness()
        const replacement = { ...harness.database, animationSpeed: 0.7 }
        harness.replaceDatabase.mockImplementationOnce(() => { throw new Error('projection failed') })
        if (mode === 'activation') {
            await expect(harness.runtime.withPausedPersistentWrites('binding-activation', async (token) => {
                harness.runtime.beginActivatedLibraryGuard(token)
                harness.nativeCommit(replacement)
                await harness.runtime.refreshActivatedLibraryUnderPause(token)
            })).rejects.toThrow('projection failed')
        } else {
            const token = await harness.runtime.capturePersistentMutationToken('restore')
            const fence = await harness.runtime.acquireDestructiveReplacementFence(token)
            expect((await fence.refreshCommittedWorkingSet(harness.nativeCommit(replacement))).projection).toBe('refresh-required')
            fence.release()
        }
        expect(harness.afterRemoteRootChange).not.toHaveBeenCalled()
        expect(harness.database.animationSpeed).not.toBe(0.7)
    })
})

describe('upstream import activation pause', () => {
    it('holds writes through activation and installs current native baselines before release', async () => {
        const harness = await createHarness()
        const admission = acquireUpstreamImportPause(harness.runtime, 'upstream-import')
        const queued = vi.fn(async () => 1)
        const operation = harness.runtime.runStorageOnlyMutation(queued).catch(error => error)
        const pause = await admission
        harness.nativeCommit(replacement('Imported library'))
        expect(() => harness.runtime.markPersistentDataDirty(1)).toThrow(PersistentMutationFencedError)
        await expect(pause.fence.refreshCommittedWorkingSet(2)).resolves.toEqual({kind:'committed',revision:2,projection:'applied'})
        pause.complete()
        await pause.finish()
        expect(await operation).toBeInstanceOf(PersistentMutationFencedError)
        expect(queued).not.toHaveBeenCalled()
        expect(harness.database.username).toBe('Imported library')
        await harness.runtime.flushPendingDataLocally('installed-baselines')
        expect(harness.store.commit).not.toHaveBeenCalled()
    })

    it('retains the guard across failed strict refresh and recovers current native content', async () => {
        const harness = await createHarness()
        const admission = acquireUpstreamImportPause(harness.runtime, 'upstream-import')
        const queued = vi.fn(async () => 1)
        const operation = harness.runtime.runStorageOnlyMutation(queued).catch(error => error)
        const pause = await admission
        harness.nativeCommit(replacement('Imported library'))
        harness.replaceDatabase.mockImplementationOnce(() => { throw new Error('projection failed') })
        await expect(pause.fence.refreshCommittedWorkingSet(2)).rejects.toThrow('projection failed')
        await pause.finish()
        expect(await operation).toBeInstanceOf(PersistentMutationFencedError)
        expect(queued).not.toHaveBeenCalled()
        expect(() => harness.runtime.markPersistentDataDirty(1)).toThrow(PersistentMutationFencedError)
        await expect(harness.runtime.retryCommittedWorkingSetRefresh()).resolves.toEqual({kind:'committed',revision:2,projection:'applied'})
        expect(harness.database.username).toBe('Imported library')
        await harness.runtime.flushPendingDataLocally('recovered-baselines')
        expect(harness.store.commit).not.toHaveBeenCalled()
    })

    it('releases an unchanged activation only after binding and exact native revision proof', async () => {
        const harness = await createHarness()
        const pause = await acquireUpstreamImportPause(harness.runtime, 'preactivation')
        const proof = vi.fn(async () => {})
        await pause.abortUnchanged(proof)
        expect(proof).toHaveBeenCalledOnce()
        const allowed = vi.fn(async () => 1)
        await harness.runtime.runStorageOnlyMutation(allowed)
        expect(allowed).toHaveBeenCalledOnce()
        const uncertain = await acquireUpstreamImportPause(harness.runtime, 'uncertain')
        harness.nativeCommit(replacement('Activated library'))
        await expect(uncertain.abortUnchanged(proof)).rejects.toThrow(PersistentMutationFencedError)
        expect(() => harness.runtime.markPersistentDataDirty(1)).toThrow(PersistentMutationFencedError)
    })

    it.each(['not-applied', 'committed'] as const)('settles a %s external application after an unknown outcome without fencing the library', async (outcome) => {
        const harness = await createHarness()
        const pause = await acquireUpstreamImportPause(harness.runtime, 'external-storage-restore')
        const proof = vi.fn(async () => {})
        const confirm = vi.fn(async (): Promise<ExternalApplicationConfirmation> => outcome === 'committed'
            ? { kind: 'committed', revision: harness.nativeCommit(replacement('Restored library')) }
            : { kind: 'not-applied', error: new Error('Restore stopped') })
        confirm.mockRejectedValueOnce(new Error('Restore outcome unknown'))
        await expect(runExternalApplication({
            jobId: 'synthetic-restore',
            fence: pause.fence,
            confirm,
            refreshReleased: async () => { throw new Error('Unexpected released refresh') },
            afterRefresh: async () => { pause.complete() },
            settled: () => outcome === 'committed' ? pause.finish() : pause.abortUnchanged(proof),
        })).rejects.toThrow('outcome unknown')
        expect(() => harness.runtime.markPersistentDataDirty(1)).toThrow(PersistentMutationFencedError)

        const retry = retryExternalApplication()
        if (outcome === 'committed') await retry
        else await expect(retry).rejects.toThrow('Restore stopped')
        expect(hasPendingExternalApplication()).toBe(false)
        expect(harness.onBackgroundError).not.toHaveBeenCalled()
        expect(harness.onWorkingSetRefreshRequired).not.toHaveBeenCalledWith(expect.any(Number))
        expect(harness.runtime.pendingWorkingSetRefreshRevision).toBeNull()
        expect(harness.database.username).toBe(outcome === 'committed' ? 'Restored library' : 'Fixture')
        const allowed = vi.fn(async (revision: number) => revision)
        await harness.runtime.runStorageOnlyMutation(allowed)
        expect(allowed).toHaveBeenCalledOnce()
        await (await acquireUpstreamImportPause(harness.runtime, 'next-activation')).abortUnchanged(proof)
    })
})

function replacement(username = 'Committed replacement'): Database {
    return { ...makeDatabase(), username }
}

describe('committed apply outcomes', () => {
    it('preserves the pending official revision while recovering its committed projection', async () => {
        const publication = { publish: vi.fn(async () => undefined), dispose: vi.fn(async () => undefined) }
        const pin = vi.fn(async () => publication)
        const harness = await createHarness({ pin })
        harness.replaceDatabase.mockImplementationOnce(() => { throw new Error('projection failed') })
        await expect(harness.runtime.replacePersistentDatabase(replacement(), 'pending-publication', {
            publishOfficial: true,
        })).resolves.toEqual({ kind: 'committed', revision: 2, projection: 'refresh-required' })
        expect(harness.runtime.hasPendingOfficialPublication()).toBe(true)

        await expect(harness.runtime.retryCommittedWorkingSetRefresh()).resolves.toEqual({
            kind: 'committed', revision: 2, projection: 'applied',
        })
        expect(harness.runtime.hasPendingOfficialPublication()).toBe(true)
        expect(pin).not.toHaveBeenCalled()
        await harness.runtime.publishCurrentOfficialRevision()
        expect(pin).toHaveBeenCalledExactlyOnceWith(2)
        expect(publication.publish).toHaveBeenCalledOnce()
        expect(harness.runtime.hasPendingOfficialPublication()).toBe(false)
        expect(harness.store.replaceFromDatabase).toHaveBeenCalledOnce()
        expect(harness.store.commit).not.toHaveBeenCalled()
    })

    it('retires an older pending official revision when recovery adopts newer native content', async () => {
        const pin = vi.fn()
        const harness = await createHarness({ pin })
        harness.replaceDatabase.mockImplementationOnce(() => { throw new Error('projection failed') })
        await harness.runtime.replacePersistentDatabase(replacement(), 'pending-publication', {
            publishOfficial: true,
        })
        harness.nativeCommit(replacement('Newer native authority'))

        await expect(harness.runtime.retryCommittedWorkingSetRefresh()).resolves.toEqual({
            kind: 'committed', revision: 3, projection: 'applied',
        })
        expect(harness.runtime.hasPendingOfficialPublication()).toBe(false)
        expect(pin).not.toHaveBeenCalled()
        expect(harness.database.username).toBe('Newer native authority')
        expect(harness.store.replaceFromDatabase).toHaveBeenCalledOnce()
        expect(harness.store.commit).not.toHaveBeenCalled()
    })

    it('keeps a scoped preset commit successful when its production projection callback fails', async () => {
        const harness = await createHarness()
        const failure = new Error('preset projection failed')
        harness.publishPresetWorkingSet.mockImplementationOnce(() => { throw failure })
        harness.store.queryPresets = vi.fn(async () => ({ revision: 1, items: [] }))
        vi.mocked(harness.store.commit).mockImplementation(async (batch) => ({
            revision: harness.nativeCommit({
                ...harness.durable,
                ...applyRootMutations(harness.durable, batch.rootMutations ?? []),
                ...batch.root,
                botPresets: batch.replacePresets ?? [],
            } as Database),
        }))

        await expect(harness.runtime.mutatePersistentPresets('preset-settings', (state) => {
            state.root.username = 'Scoped durable write'
        })).resolves.toBeUndefined()
        expect(harness.runtime.revision).toBe(2)
        expect(harness.runtime.pendingWorkingSetRefreshRevision).toBe(2)
        expect(harness.onBackgroundError).toHaveBeenCalledWith(failure)
        expect(() => harness.runtime.assertPersistentMutationAllowed()).toThrow(PersistentMutationFencedError)
        await expect(harness.runtime.retryCommittedWorkingSetRefresh()).resolves.toEqual({
            kind: 'committed', revision: 2, projection: 'applied',
        })
        expect(harness.database.username).toBe('Scoped durable write')
        expect(harness.store.commit).toHaveBeenCalledOnce()
        expect(harness.store.replaceFromDatabase).not.toHaveBeenCalled()
    })

    it('returns the confirmed revision after one replacement with no compensating content write', async () => {
        const { runtime, store, database } = await createHarness()
        const input = replacement()

        await expect(runtime.replacePersistentDatabase(input, 'local-replacement')).resolves.toEqual({
            kind: 'committed', revision: 2, projection: 'applied',
        })

        expect(runtime.revision).toBe(2)
        expect(runtime.pendingWorkingSetRefreshRevision).toBeNull()
        expect(store.replaceFromDatabase).toHaveBeenCalledOnce()
        expect(store.commit).not.toHaveBeenCalled()
        expect(store.acquireRevision).not.toHaveBeenCalled()
        expect(database.username).toBe('Fixture')
    })

    it('committed_projection_failure_is_not_a_failed_save and refreshes through PDS reads only', async () => {
        const harness = await createHarness()
        const { runtime, store, replaceDatabase } = harness
        const failure = new Error('projection failed')
        replaceDatabase.mockImplementationOnce(() => { throw failure })
        const oldEpoch = runtime.getStorageAuthorityEpoch()
        const replacing = runtime.replacePersistentDatabase(replacement(), 'failed-projection')
        await expect(replacing).resolves.toEqual({
            kind: 'committed', revision: 2, projection: 'refresh-required',
        })
        const staleWriter = vi.fn(async () => 3)
        expect(() => runtime.runStorageOnlyMutation(staleWriter))
            .toThrow(PersistentMutationFencedError)
        expect(staleWriter).not.toHaveBeenCalled()
        expect(harness.durable.username).toBe('Committed replacement')
        expect(harness.database.username).toBe('Fixture')
        expect(runtime.revision).toBe(2)
        expect(runtime.pendingWorkingSetRefreshRevision).toBe(2)
        expect(() => runtime.markPersistentDataDirty(1)).toThrow(PersistentMutationFencedError)
        expect(() => runtime.assertPersistentMutationAllowed()).toThrow(PersistentMutationFencedError)
        expect(harness.onBackgroundError).toHaveBeenCalledWith(failure)

        await expect(runtime.retryCommittedWorkingSetRefresh()).resolves.toEqual({
            kind: 'committed', revision: 2, projection: 'applied',
        })
        expect(runtime.pendingWorkingSetRefreshRevision).toBeNull()
        expect(harness.database.username).toBe('Committed replacement')
        expect(() => runtime.assertPersistentMutationAllowed()).not.toThrow()
        expect(() => runtime.assertPersistentMutationAllowed(oldEpoch)).toThrow(PersistentMutationFencedError)
        await runtime.flushPendingDataLocally('after-read-only-refresh')
        expect(store.commit).not.toHaveBeenCalled()
        expect(store.replaceFromDatabase).toHaveBeenCalledOnce()
        expect(store.materializeDatabase).not.toHaveBeenCalled()
        expect(harness.leases[0].release).toHaveBeenCalledOnce()
        expect(harness.onWorkingSetRefreshRequired.mock.calls).toEqual([[2], [null]])
    })

    it('runs the source continuation after the real recovery install advances authority', async () => {
        const harness = await createHarness()
        harness.replaceDatabase.mockImplementationOnce(() => {
            throw new Error('projection failed')
        })
        await expect(
            harness.runtime.replacePersistentDatabase(replacement(), 'failed-projection'),
        ).resolves.toEqual({
            kind: 'committed', revision: 2, projection: 'refresh-required',
        })
        const authorityBeforeRecovery = harness.runtime.getStorageAuthorityEpoch()
        const continuation = vi.fn(async () => {})
        registerCommittedWorkingSetContinuation(
            2,
            harness.runtime,
            authorityBeforeRecovery,
            continuation,
        )

        await expect(
            retryCommittedWorkingSetRefreshWithContinuation(harness.runtime),
        ).resolves.toEqual({
            kind: 'committed', revision: 2, projection: 'applied',
        })

        expect(harness.runtime.getStorageAuthorityEpoch()).toBe(
            authorityBeforeRecovery + 1,
        )
        expect(continuation).toHaveBeenCalledOnce()
        expect(harness.store.replaceFromDatabase).toHaveBeenCalledOnce()
        expect(harness.store.commit).not.toHaveBeenCalled()
    })

    it('keeps the guard through another failed refresh without repeating the original replacement', async () => {
        const { runtime, store, replaceDatabase } = await createHarness()
        replaceDatabase.mockImplementationOnce(() => { throw new Error('projection failed') })
        await runtime.replacePersistentDatabase(replacement(), 'failed-projection')
        vi.mocked(store.acquireRevision).mockRejectedValueOnce(new Error('read failed'))

        await expect(runtime.retryCommittedWorkingSetRefresh()).resolves.toEqual({
            kind: 'committed', revision: 2, projection: 'refresh-required',
        })
        expect(runtime.pendingWorkingSetRefreshRevision).toBe(2)
        expect(() => runtime.assertPersistentMutationAllowed()).toThrow(PersistentMutationFencedError)
        await expect(runtime.retryCommittedWorkingSetRefresh()).resolves.toEqual({
            kind: 'committed', revision: 2, projection: 'applied',
        })
        expect(store.commit).not.toHaveBeenCalled()
        expect(store.replaceFromDatabase).toHaveBeenCalledOnce()
    })

    it('recovers the latest confirmed revision when another native commit advanced the store', async () => {
        const harness = await createHarness()
        harness.replaceDatabase.mockImplementationOnce(() => { throw new Error('projection failed') })
        await harness.runtime.replacePersistentDatabase(replacement(), 'failed-projection')
        expect(harness.nativeCommit(replacement('Newer native commit'))).toBe(3)

        await expect(harness.runtime.retryCommittedWorkingSetRefresh()).resolves.toEqual({
            kind: 'committed', revision: 3, projection: 'applied',
        })
        expect(harness.database.username).toBe('Newer native commit')
        expect(harness.store.acquireRevision).toHaveBeenCalledWith(3)
        expect(harness.store.replaceFromDatabase).toHaveBeenCalledOnce()
        expect(harness.store.commit).not.toHaveBeenCalled()
    })

    it('does not install a delayed projection after navigation has changed', async () => {
        const harness = await createHarness()
        const { runtime, store } = harness
        const token = await runtime.capturePersistentMutationToken('native-prepare', { publishOfficial: false })
        const fence = await runtime.acquireDestructiveReplacementFence(token)
        const revision = harness.nativeCommit(replacement())
        const started = deferred<void>()
        const finish = deferred<void>()
        const acquire = store.acquireRevision.bind(store)
        vi.mocked(store.acquireRevision).mockImplementationOnce(async (requested) => {
            started.resolve()
            await finish.promise
            return acquire(requested)
        })
        const refreshing = fence.refreshCommittedWorkingSet(revision)
        await started.promise
        runtime.invalidateNavigation()
        finish.resolve()

        await expect(refreshing).resolves.toEqual({
            kind: 'committed', revision, projection: 'refresh-required',
        })
        expect(harness.replaceDatabase).not.toHaveBeenCalled()
        fence.release()
        expect(() => runtime.assertPersistentMutationAllowed()).toThrow(PersistentMutationFencedError)
        await expect(runtime.retryCommittedWorkingSetRefresh()).resolves.toMatchObject({ projection: 'applied' })
        expect(store.commit).not.toHaveBeenCalled()
        expect(store.replaceFromDatabase).not.toHaveBeenCalled()
    })

    it('refreshes two committed revisions under one held fence', async () => {
        const harness = await createHarness()
        const { runtime } = harness
        const token = await runtime.capturePersistentMutationToken('exit-fence', { publishOfficial: false })
        const fence = await runtime.acquireDestructiveReplacementFence(token)

        const first = harness.nativeCommit(replacement('First remote commit'))
        await expect(fence.refreshCommittedWorkingSet(first)).resolves.toEqual({
            kind: 'committed', revision: first, projection: 'applied',
        })
        const second = harness.nativeCommit(replacement('Second remote commit'))
        await expect(fence.refreshCommittedWorkingSet(second)).resolves.toEqual({
            kind: 'committed', revision: second, projection: 'applied',
        })
        expect(harness.database.username).toBe('Second remote commit')
        expect(() => runtime.assertPersistentMutationAllowed()).toThrow(PersistentMutationFencedError)
        fence.release()
        expect(() => runtime.assertPersistentMutationAllowed()).not.toThrow()
        expect(runtime.revision).toBe(second)
    })

    it('keeps publication failure separate from the completed local replacement', async () => {
        const failure = new Error('official service unavailable')
        const publication = { publish: vi.fn(async () => { throw failure }), dispose: vi.fn() }
        const pin = vi.fn(async () => publication)
        const { runtime, store } = await createHarness({ pin })

        await expect(runtime.replacePersistentDatabase(replacement(), 'local-before-publication', {
            publishOfficial: true,
        })).resolves.toEqual({ kind: 'committed', revision: 2, projection: 'applied' })
        expect(pin).not.toHaveBeenCalled()
        expect(runtime.hasPendingOfficialPublication()).toBe(true)
        await expect(runtime.publishCurrentOfficialRevision()).rejects.toBe(failure)
        expect(pin).toHaveBeenCalledWith(2)
        expect(runtime.hasPendingOfficialPublication()).toBe(true)
        expect(runtime.revision).toBe(2)
        expect(runtime.pendingWorkingSetRefreshRevision).toBeNull()
        await runtime.flushPendingDataLocally('local-after-publication-failure')
        expect(store.replaceFromDatabase).toHaveBeenCalledOnce()
        expect(store.commit).not.toHaveBeenCalled()
    })

    it('refuses destructive application during generation without cancelling the generation or retaining a fence', async () => {
        const harness = await createHarness()
        const token = await harness.runtime.capturePersistentMutationToken('prepare', { publishOfficial: false })
        harness.generating = true

        await expect(harness.runtime.acquireDestructiveReplacementFence(token))
            .rejects.toBeInstanceOf(PersistentMutationFencedError)
        expect(() => harness.runtime.assertPersistentMutationAllowed()).not.toThrow()
        expect(harness.store.commit).not.toHaveBeenCalled()
        expect(harness.store.replaceFromDatabase).not.toHaveBeenCalled()
    })
})


describe('activated-library guards', () => {
    it('fences queued explicit and observer writes before a failed activation pause releases', async () => {
        const harness = await createHarness()
        const entered = deferred<void>()
        const activate = deferred<void>()
        const queuedMutation = vi.fn(async () => (await harness.store.commit({expectedRevision: 2})).revision)
        const pause = harness.runtime.withPausedPersistentWrites('activate', async (token) => {
            entered.resolve()
            await activate.promise
            harness.runtime.beginActivatedLibraryGuard(token)
            harness.nativeCommit(replacement('Activated library'))
            harness.replaceDatabase.mockImplementationOnce(() => { throw new Error('refresh failed') })
            await harness.runtime.refreshActivatedLibraryUnderPause(token)
        })
        await entered.promise
        harness.database.username = 'Stale observer edit'
        const explicit = harness.runtime.runStorageOnlyMutation(queuedMutation).catch((error: unknown) => error)
        const observer = harness.runtime.flushPendingDataLocally('queued-observer').catch((error: unknown) => error)
        activate.resolve()
        await expect(pause).rejects.toThrow('refresh failed')
        expect(await explicit).toBeInstanceOf(PersistentMutationFencedError)
        expect(await observer).toBeInstanceOf(PersistentMutationFencedError)
        expect(queuedMutation).not.toHaveBeenCalled()
        expect(() => harness.runtime.markPersistentDataDirty(1)).toThrow(PersistentMutationFencedError)
        await expect(harness.runtime.commitPersistentUnitIntent('stale', [{key: JSON.stringify(['root', 'username']), type: 'set', value: 'Stale explicit edit'}])).rejects.toThrow(PersistentMutationFencedError)
        await expect(harness.runtime.initializeActiveWorkingSet(harness.database)).rejects.toThrow(PersistentMutationFencedError)
        expect(harness.store.commit).not.toHaveBeenCalled()
        expect(harness.runtime.pendingWorkingSetRefreshRevision).toBe(1)

        await expect(harness.runtime.retryCommittedWorkingSetRefresh()).resolves.toEqual({kind: 'committed', revision: 2, projection: 'applied'})
        expect(harness.database.username).toBe('Activated library')
        expect(harness.runtime.pendingWorkingSetRefreshRevision).toBeNull()
        await harness.runtime.flushPendingDataLocally('recovery-baseline')
        expect(harness.store.commit).not.toHaveBeenCalled()
        vi.mocked(harness.store.commit).mockImplementation(async (batch) => {
            const root = applyRootMutations(harness.durable, batch.rootMutations ?? [])
            for (const mutation of batch.unitMutations ?? []) {
                const path = JSON.parse(mutation.key) as string[]
                if (path[0] === 'root' && mutation.type === 'set') Object.assign(root, {[path[1]]: mutation.value})
            }
            return {revision: harness.nativeCommit({...harness.durable, ...root} as Database)}
        })
        harness.database.username = 'Fresh edit'
        harness.runtime.markPersistentDataDirty(1)
        await harness.runtime.flushPendingDataLocally('fresh')
        expect(harness.store.commit).toHaveBeenCalledOnce()
        expect(harness.durable.username).toBe('Fresh edit')
    })

    it('retains the guard when activation or its resulting revision is uncertain', async () => {
        const harness = await createHarness()
        await expect(harness.runtime.withPausedPersistentWrites('uncertain', async (token) => {
            harness.runtime.beginActivatedLibraryGuard(token)
            harness.nativeCommit(replacement('Native activation succeeded'))
            throw new Error('native result lost')
        })).rejects.toThrow('native result lost')
        expect(() => harness.runtime.assertPersistentMutationAllowed()).toThrow(PersistentMutationFencedError)
        expect(harness.store.commit).not.toHaveBeenCalled()
        await harness.runtime.retryCommittedWorkingSetRefresh()
        expect(harness.database.username).toBe('Native activation succeeded')
        expect(() => harness.runtime.assertPersistentMutationAllowed()).not.toThrow()
    })

    it('releases an unchanged preactivation failure only after checking the admission revision', async () => {
        const harness = await createHarness()
        let guard!: ReturnType<typeof harness.runtime.beginActivatedLibraryGuard>
        await expect(harness.runtime.withPausedPersistentWrites('preactivation', async (token) => {
            guard = harness.runtime.beginActivatedLibraryGuard(token)
            throw new Error('not activated')
        })).rejects.toThrow('not activated')
        expect(() => harness.runtime.assertPersistentMutationAllowed()).toThrow(PersistentMutationFencedError)
        await guard.abortUnchanged()
        expect(harness.runtime.pendingWorkingSetRefreshRevision).toBeNull()
        expect(() => harness.runtime.assertPersistentMutationAllowed()).not.toThrow()
        await harness.runtime.flushPendingDataLocally('unchanged-baseline')
        expect(harness.store.commit).not.toHaveBeenCalled()
    })

    it('rejects unchanged abort when the native revision advanced under the same writer', async () => {
        const harness = await createHarness()
        let guard!: ReturnType<typeof harness.runtime.beginActivatedLibraryGuard>
        await expect(harness.runtime.withPausedPersistentWrites('bound-restore', async (token) => {
            guard = harness.runtime.beginActivatedLibraryGuard(token)
            harness.nativeCommit(replacement('Restored library'))
            throw new Error('restore result unavailable')
        })).rejects.toThrow('restore result unavailable')
        await expect(guard.abortUnchanged()).rejects.toThrow(PersistentMutationFencedError)
        expect(() => harness.runtime.assertPersistentMutationAllowed()).toThrow(PersistentMutationFencedError)
        expect(harness.store.commit).not.toHaveBeenCalled()
    })

    it('keeps partial projection adoption fenced until a strict cursor acknowledgement succeeds', async () => {
        const harness = await createHarness()
        const cursor = vi.fn().mockRejectedValueOnce(new Error('cursor failed')).mockRejectedValueOnce(new Error('retry cursor failed')).mockResolvedValue(undefined)
        harness.store.commitWorkingSetChangeCursor = cursor
        let guard!: ReturnType<typeof harness.runtime.beginActivatedLibraryGuard>
        await expect(harness.runtime.withPausedPersistentWrites('cursor', async (token) => {
            guard = harness.runtime.beginActivatedLibraryGuard(token)
            harness.nativeCommit(replacement('Activated projection'))
            await harness.runtime.refreshActivatedLibraryUnderPause(token)
            guard.complete()
        })).rejects.toThrow('cursor failed')
        expect(harness.database.username).toBe('Activated projection')
        expect(harness.runtime.pendingWorkingSetRefreshRevision).toBe(2)
        expect(() => guard.complete()).toThrow(PersistentMutationFencedError)
        expect(() => harness.runtime.markPersistentDataDirty(1)).toThrow(PersistentMutationFencedError)
        await expect(harness.runtime.retryCommittedWorkingSetRefresh()).resolves.toEqual({kind: 'committed', revision: 2, projection: 'refresh-required'})
        expect(harness.runtime.pendingWorkingSetRefreshRevision).toBe(2)
        expect(() => harness.runtime.assertPersistentMutationAllowed()).toThrow(PersistentMutationFencedError)
        await expect(harness.runtime.retryCommittedWorkingSetRefresh()).resolves.toEqual({kind: 'committed', revision: 2, projection: 'applied'})
        expect(cursor).toHaveBeenCalledTimes(3)
        expect(harness.runtime.pendingWorkingSetRefreshRevision).toBeNull()
        expect(harness.store.commit).not.toHaveBeenCalled()
    })

    it('requires the exact active pause token and completes only after projection adoption', async () => {
        const harness = await createHarness()
        await harness.runtime.withPausedPersistentWrites('successful', async (token) => {
            expect(() => harness.runtime.beginActivatedLibraryGuard({...token})).toThrow(PersistentMutationFencedError)
            const guard = harness.runtime.beginActivatedLibraryGuard(token)
            expect(() => guard.complete()).toThrow(PersistentMutationFencedError)
            harness.nativeCommit(replacement('New library'))
            await harness.runtime.refreshActivatedLibraryUnderPause(token)
            guard.complete()
        })
        expect(harness.runtime.pendingWorkingSetRefreshRevision).toBeNull()
        expect(harness.database.username).toBe('New library')
        expect(() => harness.runtime.assertPersistentMutationAllowed()).not.toThrow()
        await harness.runtime.flushPendingDataLocally('new-baseline')
        expect(harness.store.commit).not.toHaveBeenCalled()
    })

    it('does not fence an ordinary paused operation failure without library activation', async () => {
        const harness = await createHarness()
        await expect(harness.runtime.withPausedPersistentWrites('ordinary-clock-error', async () => {
            throw new Error('clock admission failed')
        })).rejects.toThrow('clock admission failed')
        expect(harness.runtime.pendingWorkingSetRefreshRevision).toBeNull()
        expect(() => harness.runtime.assertPersistentMutationAllowed()).not.toThrow()
        await harness.runtime.flushPendingDataLocally('ordinary-retry')
        expect(harness.store.commit).not.toHaveBeenCalled()
    })

    it('retains an adopted projection guard when the owner did not complete inside the pause', async () => {
        const harness = await createHarness()
        let guard!: ReturnType<typeof harness.runtime.beginActivatedLibraryGuard>
        await harness.runtime.withPausedPersistentWrites('validation-incomplete', async (token) => {
            guard = harness.runtime.beginActivatedLibraryGuard(token)
            harness.nativeCommit(replacement('Adopted but unvalidated'))
            await harness.runtime.refreshActivatedLibraryUnderPause(token)
        })
        expect(() => guard.complete()).toThrow(PersistentMutationFencedError)
        expect(() => harness.runtime.assertPersistentMutationAllowed()).toThrow(PersistentMutationFencedError)
        await expect(harness.runtime.retryCommittedWorkingSetRefresh()).resolves.toEqual({kind: 'committed', revision: 2, projection: 'applied'})
        expect(harness.runtime.pendingWorkingSetRefreshRevision).toBeNull()
        expect(harness.store.commit).not.toHaveBeenCalled()
    })


    it('fully adopts and acknowledges guarded recovery while the previous projected working set reports generation', async () => {
        const harness = await createHarness(undefined, true)
        const cursor = vi.fn(async (_revision: number) => undefined)
        harness.store.commitWorkingSetChangeCursor = cursor
        await harness.runtime.withPausedPersistentWrites('initial-activation', async (token) => {
            const guard = harness.runtime.beginActivatedLibraryGuard(token)
            harness.nativeCommit(replacement('First activated projection'))
            await harness.runtime.refreshActivatedLibraryUnderPause(token)
            guard.complete()
        })
        harness.generating = true
        await expect(harness.runtime.withPausedPersistentWrites('next-activation', async (token) => {
            harness.runtime.beginActivatedLibraryGuard(token)
            harness.nativeCommit(replacement('New complete projection'))
            throw new Error('activation result unavailable')
        })).rejects.toThrow('activation result unavailable')
        await expect(harness.runtime.retryCommittedWorkingSetRefresh()).resolves.toEqual({kind: 'committed', revision: 3, projection: 'applied'})
        expect(harness.database.username).toBe('New complete projection')
        expect(cursor.mock.calls).toEqual([[2], [3]])
        expect(harness.runtime.pendingWorkingSetRefreshRevision).toBeNull()
        expect(harness.store.commit).not.toHaveBeenCalled()
    })

})
