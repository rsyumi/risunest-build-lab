import { describe, expect, it, vi } from 'vitest'
import { createPersistentStoreHarness } from '../../../tests/support/persistentStoreHarness'
import type { Database } from './database.svelte'
import { createMutationGatedPersistentDataStore } from './mutationGatedPersistentDataStore'
import type { PersistentDataStore, WorkingSetCommit } from './persistentDataStore'
import { createStoreStub } from './persistentDataStore.testSupport'
import {
    createInRealmStorageLockManager,
    createStorageMutationGate,
    type StorageMutationGate,
} from './storageMutationGate'

function deferred<T = void>() {
    let resolve!: (value: T | PromiseLike<T>) => void
    const promise = new Promise<T>((complete) => { resolve = complete })
    return { promise, resolve }
}

describe('createMutationGatedPersistentDataStore', () => {
    it('forwards optional current binding reads without acquiring a mutation gate', async () => {
        const store: PersistentDataStore = (await createPersistentStoreHarness()).store
        store.lwwBindingState = vi.fn(async () => ({targetAuthority:'17'}))
        const gate = {runWrite:vi.fn(),runTransition:vi.fn()} as unknown as StorageMutationGate
        const gated = createMutationGatedPersistentDataStore(store, gate)
        expect(await gated.lwwBindingState!()).toEqual({targetAuthority:'17'})
        expect(store.lwwBindingState).toHaveBeenCalledOnce()
        expect(gate.runWrite).not.toHaveBeenCalled()
        expect(gate.runTransition).not.toHaveBeenCalled()
        expect(createMutationGatedPersistentDataStore((await createPersistentStoreHarness()).store, gate).lwwBindingState).toBeUndefined()
    })
    it('gates activation on exposed native stages without nesting the normal replacement gate',async()=>{
        const harness=await createPersistentStoreHarness()
        const store:PersistentDataStore=harness.store
        const replace=harness.spy('replaceFromDatabase')
        const activate=vi.fn(async()=>({revision:2})),abort=vi.fn(async()=>{})
        store.stageDatabaseReplacement=vi.fn(async()=>({activate,abort}))
        let held=false
        const gate={runWrite:async<T>(operation:()=>Promise<T>)=>operation(),runKeyedWrite:async<T>(_key:string,operation:()=>Promise<T>)=>operation(),
            runTransition:vi.fn(async<T>(operation:()=>Promise<T>)=>{expect(held).toBe(false);held=true;try{return await operation()}finally{held=false}})}
        activate.mockImplementation(async()=>{expect(held).toBe(true);return {revision:2}})
        const gated=createMutationGatedPersistentDataStore(store,gate as StorageMutationGate)
        const stage=await gated.stageDatabaseReplacement!({characters:[]} as unknown as Database,1)
        expect(gate.runTransition).not.toHaveBeenCalled()
        await stage.activate();expect(gate.runTransition).toHaveBeenCalledOnce()
        await gated.replaceFromDatabase({characters:[]} as unknown as Database,1)
        expect(gate.runTransition).toHaveBeenCalledTimes(2)
        expect(replace).not.toHaveBeenCalled()
    })

    it('forwards optional metadata reads with the original receiver without taking a mutation gate', async () => {
        const harness = await createPersistentStoreHarness()
        const store = harness.store
        const query = { characterId: 'char-a', conversationId: 'chat-a', startIndex: 0, limit: 64 }
        const result = { revision: 1, value: { messages: [] } } as any
        harness.spy('readConversationMessageMetadataWindow').mockImplementation(async function (this: PersistentDataStore, input) {
            expect(this).toBe(store)
            expect(input).toBe(query)
            return result
        })
        const gate = createStorageMutationGate({ locks: createInRealmStorageLockManager() })
        const write = vi.spyOn(gate, 'runWrite')
        const gated = createMutationGatedPersistentDataStore(store, gate)
        expect(await gated.readConversationMessageMetadataWindow!(query)).toBe(result)
        expect(write).not.toHaveBeenCalled()
        // The read is optional in the store contract, and the real store always has it.
        expect(createMutationGatedPersistentDataStore(createStoreStub({}), gate).readConversationMessageMetadataWindow).toBeUndefined()
    })

    it('gates ordinary commits and full replacements while preserving exact inputs and results', async () => {
        const harness = await createPersistentStoreHarness()
        const calls: string[] = []
        const gate = {
            runWrite: vi.fn(async <T>(operation: () => Promise<T>) => {
                calls.push('write-gate')
                return operation()
            }),
            runKeyedWrite: vi.fn(async <T>(_key: string, operation: () => Promise<T>) => operation()),
            runTransition: vi.fn(async <T>(operation: () => Promise<T>) => {
                calls.push('transition-gate')
                return operation()
            }),
        } as StorageMutationGate
        const commit = {
            expectedRevision: 3,
            characterDetails: [{ type: 'character', chaId: 'char-a', name: 'Character' }],
            pluginStorage: [{ type: 'set', owner: 'test-plugin', key: 'plugin', value: true }],
        } as WorkingSetCommit
        const database = { username: 'Fixture', characters: [] } as unknown as Database
        const aliases: Parameters<PersistentDataStore['replaceFromDatabase']>[2] = []
        const values = [{ owner: 'test-plugin', key: 'key', value: 'value' }]
        const commitResult = { revision: 4 }
        const replacementResult = { revision: 5 }
        harness.spy('commit').mockImplementation(async (input) => {
            calls.push('commit')
            expect(input).toBe(commit)
            return commitResult
        })
        harness.spy('replaceFromDatabase').mockImplementation(async (input, revision, assets, pluginValues) => {
            calls.push('replace')
            expect(input).toBe(database)
            expect(revision).toBe(4)
            expect(assets).toBe(aliases)
            expect(pluginValues).toBe(values)
            return replacementResult
        })
        const gated = createMutationGatedPersistentDataStore(harness.store, gate)

        await expect(gated.commit(commit)).resolves.toBe(commitResult)
        await expect(gated.replaceFromDatabase(database, 4, aliases, values)).resolves.toBe(replacementResult)

        expect(calls).toEqual(['write-gate', 'commit', 'transition-gate', 'replace'])
        expect(gate.runWrite).toHaveBeenCalledOnce()
        expect(gate.runTransition).toHaveBeenCalledOnce()
    })

    it('transition_does_not_reenter_shared_gate', async () => {
        const harness = await createPersistentStoreHarness()
        const store = harness.store
        const gate = createStorageMutationGate({ locks: createInRealmStorageLockManager() })
        const shared = vi.spyOn(gate, 'runWrite')
        const exclusive = vi.spyOn(gate, 'runTransition')
        const started = deferred()
        const finishFirstWrite = deferred()
        const calls: string[] = []
        harness.spy('commit').mockImplementation(async ({ expectedRevision }) => {
            calls.push(`write:${expectedRevision}`)
            if (expectedRevision === 0) {
                started.resolve()
                await finishFirstWrite.promise
            }
            calls.push(`written:${expectedRevision + 1}`)
            return { revision: expectedRevision + 1 }
        })
        harness.spy('replaceFromDatabase').mockImplementation(async (_database, revision) => {
            calls.push('transition')
            // The replacement owns the exclusive permit and uses its internal store.
            const result = await store.commit({ expectedRevision: revision! })
            calls.push('replaced')
            return result
        })
        const gated = createMutationGatedPersistentDataStore(store, gate)
        const first = gated.commit({ expectedRevision: 0 })
        await started.promise
        const replacement = gated.replaceFromDatabase({ characters: [] } as unknown as Database, 1)
        const last = gated.commit({ expectedRevision: 2 })

        expect(calls).toEqual(['write:0'])
        finishFirstWrite.resolve()
        await expect(Promise.all([first, replacement, last])).resolves.toEqual([
            { revision: 1 }, { revision: 2 }, { revision: 3 },
        ])
        expect(calls).toEqual([
            'write:0', 'written:1', 'transition', 'write:1', 'written:2',
            'replaced', 'write:2', 'written:3',
        ])
        expect(shared).toHaveBeenCalledTimes(2)
        expect(exclusive).toHaveBeenCalledOnce()
    })

    it('releases a failed exclusive transition before the next shared writer', async () => {
        const harness = await createPersistentStoreHarness()
        const store = harness.store
        const gate = createStorageMutationGate({ locks: createInRealmStorageLockManager() })
        const started = deferred()
        const finishReplacement = deferred()
        const failure = new Error('replacement failed')
        harness.spy('replaceFromDatabase').mockImplementation(async () => {
            started.resolve()
            await finishReplacement.promise
            throw failure
        })
        harness.spy('commit').mockResolvedValue({ revision: 2 })
        const gated = createMutationGatedPersistentDataStore(store, gate)
        const replacement = gated.replaceFromDatabase({ characters: [] } as unknown as Database, 1)
        const rejected = expect(replacement).rejects.toBe(failure)
        await started.promise
        const write = gated.commit({ expectedRevision: 1 })
        expect(store.commit).not.toHaveBeenCalled()
        finishReplacement.resolve()
        await rejected
        await expect(write).resolves.toEqual({ revision: 2 })
        expect(store.commit).toHaveBeenCalledOnce()
    })

    it('reads and exports without acquiring the write gate', async () => {
        const harness = await createPersistentStoreHarness({ username: 'Fixture', botPresets: [], characters: [], pluginCustomStorage: { fixture: 'value' } } as unknown as Database)
        const aliases = harness.spy('readAssetAliasesByKeys')
        const metadata = harness.spy('readConversationMetadata')
        const gate = { runWrite: vi.fn() } as unknown as StorageMutationGate
        const gated = createMutationGatedPersistentDataStore(harness.store, gate)

        await expect(gated.materializeDatabase(harness.revision)).resolves.toMatchObject({ username: 'Fixture' })
        expect((await gated.queryPluginStorage('different-owner')).items).toEqual([])
        const lease = await gated.acquireRevision(harness.revision)
        expect(lease.revision).toBe(harness.revision)
        await lease.release()
        await gated.readRoot()
        await gated.queryPresets()
        await gated.readPreset('0')
        await gated.queryPluginStorage()
        await gated.readPluginStorage('test-plugin', 'plugin')
        await gated.readConversationMetadata('char-a', 'conv-a')
        await gated.readAssetAliasesByKeys('asset', ['assets/batch.bin'])

        expect(gate.runWrite).not.toHaveBeenCalled()
        expect(aliases).toHaveBeenCalledWith('asset', ['assets/batch.bin'])
        expect(metadata).toHaveBeenCalledWith('char-a', 'conv-a')
    })

    it('preserves ordinary write error identity', async () => {
        const harness = await createPersistentStoreHarness()
        const failure = new Error('commit failed')
        harness.failNext('commit', failure)
        const gate = {
            runWrite: <T>(operation: () => Promise<T>) => operation(),
            runKeyedWrite: <T>(_key: string, operation: () => Promise<T>) => operation(),
            runTransition: <T>(operation: () => Promise<T>) => operation(),
        }
        const gated = createMutationGatedPersistentDataStore(harness.store, gate)

        await expect(gated.commit({ expectedRevision: 1 })).rejects.toBe(failure)
    })
})
