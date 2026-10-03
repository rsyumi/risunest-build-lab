import {beforeEach,describe,expect,it,vi} from 'vitest'
vi.mock('../platform',()=>({isTauri:true}))
const plugins=vi.hoisted(()=>({fencePluginExecutionForAuthorityReplacement:vi.fn(async()=>{}),invalidatePluginCachesAfterAuthorityReplacement:vi.fn(async()=>{}),restartPluginsAfterAuthorityReplacement:vi.fn(async()=>{})}))
const binding=vi.hoisted(()=>({bound:true,state:{targetAuthority:'9007199254740993'},fence:vi.fn(async()=>{}),assertAuthority:vi.fn(async()=>{}),resume:vi.fn(async()=>{})}))
vi.mock('./sync/bindingRegistry',()=>({prepareBoundLibraryReplacement:async()=>binding}))
const alerts=vi.hoisted(()=>({checkbox:vi.fn(async()=>({confirmed:true,checked:true}))}))
vi.mock('../alert',()=>({alertCheckboxConfirm:alerts.checkbox}))
vi.mock('../plugins/apiV3/v3.svelte',()=>plugins)
import type {Database} from './database.svelte'
import type {PersistentDataStore,PersistentRevisionLease} from './persistentDataStore'
import {capturePersistentPluginStorage,capturePersistentPresets,capturePersistentRoot,createPersistentDataRuntime} from './persistentDataRuntime'
import type {OfficialRevisionPublisher} from './saveCoordinator'
import {makeDatabase} from './saveCoordinator.testSupport'
import {runNativeBlockRisuSaveRestore,type NativeFileJobStatus} from './nativeFileJobs'
import {retryCommittedWorkingSetRefreshWithContinuation} from './committedWorkingSetContinuation'
async function createHarness(officialPublisher?: OfficialRevisionPublisher, captureWorkingSet = false) {
    let database = makeDatabase()
    let durable = structuredClone(database)
    let revision = 1
    let generating = false
    const leases: PersistentRevisionLease[] = []
    const replaceDatabase = vi.fn((replacement: Database) => { database = replacement })
    const onWorkingSetRefreshRequired = vi.fn()
    const onBackgroundError = vi.fn()
    const publishPresetWorkingSet = vi.fn()
    const store = {
        commitWorkingSetChangeCursor:vi.fn(async()=>{}),
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
        onWorkingSetRefreshRequired, onBackgroundError, leases,
        get database() { return database },
        get durable() { return durable },
        set generating(value: boolean) { generating = value },
        nativeCommit(replacement: Database) {
            durable = structuredClone(replacement)
            return ++revision
        },
    }
}

beforeEach(()=>{vi.clearAllMocks();plugins.restartPluginsAfterAuthorityReplacement.mockImplementation(async()=>{});binding.bound=true;binding.fence.mockClear();binding.assertAuthority.mockClear();binding.resume.mockReset();binding.resume.mockImplementation(async()=>{})})
describe('direct native upstream import publication',()=>{
    it.each(['cursor','resume'] as const)('shares concurrent recovery after adoption while %s is blocked and retains failed resume',async phase=>{
        const h=await createHarness()
        const incoming={...makeDatabase(),username:'Concurrent import'}
        vi.mocked(h.store.replaceFromDatabase).mockImplementationOnce(async database=>{
            h.nativeCommit(database)
            throw new Error('activation response lost')
        })
        await expect(h.runtime.replacePersistentDatabase(incoming,'concurrent-import',{upstreamImport:true})).rejects.toThrow()
        const originalEpoch=h.runtime.getStorageAuthorityEpoch()
        let enter!:()=>void
        let release!:()=>void
        const entered=new Promise<void>(resolve=>{enter=resolve})
        const blocked=new Promise<void>(resolve=>{release=resolve})
        if(phase==='cursor')vi.mocked(h.store.commitWorkingSetChangeCursor!).mockImplementationOnce(async()=>{enter();await blocked})
        binding.resume.mockImplementationOnce(async()=>{
            if(phase==='resume'){enter();await blocked}
            throw new Error('first adapter resume failed')
        })
        const first=h.runtime.retryCommittedWorkingSetRefresh()
        await entered
        expect(h.runtime.getStorageAuthorityEpoch()).toBeGreaterThan(originalEpoch)
        const competing=retryCommittedWorkingSetRefreshWithContinuation(h.runtime)
        release()
        await expect(first).resolves.toMatchObject({projection:'refresh-required'})
        await expect(competing).resolves.toMatchObject({projection:'refresh-required'})
        expect(binding.resume).toHaveBeenCalledOnce()
        expect(()=>h.runtime.markPersistentDataDirty(1)).toThrow()
        await expect(retryCommittedWorkingSetRefreshWithContinuation(h.runtime)).resolves.toEqual({kind:'committed',revision:2,projection:'applied'})
        expect(binding.resume).toHaveBeenCalledTimes(2)
        expect(plugins.restartPluginsAfterAuthorityReplacement).toHaveBeenCalledOnce()
        expect(h.store.replaceFromDatabase).toHaveBeenCalledOnce()
        expect(h.database.username).toBe(incoming.username)
        await expect(retryCommittedWorkingSetRefreshWithContinuation(h.runtime)).resolves.toBeNull()
    })

    it('cancels a declined upstream replacement as an abort before fencing the sync binding',async()=>{
        const h=await createHarness()
        alerts.checkbox.mockResolvedValueOnce({confirmed:false,checked:false})
        await expect(h.runtime.replacePersistentDatabase({...makeDatabase(),username:'Declined'},'declined',{upstreamImport:true})).rejects.toMatchObject({name:'AbortError'})
        expect(alerts.checkbox).toHaveBeenCalledOnce()
        expect(binding.fence).not.toHaveBeenCalled()
        expect(h.store.replaceFromDatabase).not.toHaveBeenCalled()
        expect(h.database.username).not.toBe('Declined')
    })

    it('emits the operation restart receipt only after the required native restart succeeds',async()=>{
        const h=await createHarness()
        const receipt=vi.fn(()=>{expect(plugins.restartPluginsAfterAuthorityReplacement).toHaveBeenCalledOnce()})
        await h.runtime.replacePersistentDatabase(makeDatabase(),'receipt',{upstreamImport:true,onPluginsRestarted:receipt})
        expect(receipt).toHaveBeenCalledOnce()
    })
    it.each([{expectedRevision:99},{expectedMutationGeneration:99}])('preserves snapshot expectation before direct native staging: %j',async expectation=>{
        const h=await createHarness()
        await expect(h.runtime.replacePersistentDatabase({...makeDatabase(),username:'Rejected'},'stale-snapshot',{upstreamImport:true,...expectation})).rejects.toThrow()
        expect(h.store.replaceFromDatabase).not.toHaveBeenCalled()
        expect(h.database.username).not.toBe('Rejected')
        expect(()=>h.runtime.markPersistentDataDirty(1)).not.toThrow()
        expect(binding.resume).toHaveBeenCalledOnce()
    })
    it.each(['refresh','uncertain'] as const)('retains read-only activation and resumes only after strict recovery: %s',async failure=>{
        const h=await createHarness({pin:async()=>({publish:async()=>{},dispose:async()=>{}})})
        const incoming={...makeDatabase(),username:'Imported after recovery'}
        if(failure==='refresh') h.replaceDatabase.mockImplementationOnce(()=>{throw new Error('refresh failed')})
        else vi.mocked(h.store.replaceFromDatabase).mockImplementationOnce(async database=>{
            h.nativeCommit(database)
            throw new Error('activation result unavailable')
        })
        await expect(h.runtime.replacePersistentDatabase(incoming,'upstream-import',{upstreamImport:true,publishOfficial:true})).rejects.toThrow()
        expect(()=>h.runtime.markPersistentDataDirty(1)).toThrow()
        expect(binding.resume).not.toHaveBeenCalled()
        await expect(retryCommittedWorkingSetRefreshWithContinuation(h.runtime)).resolves.toEqual({kind:'committed',revision:2,projection:'applied'})
        expect(h.database.username).toBe('Imported after recovery')
        expect(binding.resume).toHaveBeenCalledOnce()
        expect(h.runtime.hasPendingOfficialPublication()).toBe(true)
        await h.runtime.flushPendingDataLocally('recovery-baselines')
        expect(h.store.commit).not.toHaveBeenCalled()
    })
    it.each([true,false])('uses captured LWW authority and preserves explicit official publication=%s',async publishOfficial=>{
        const publication={publish:vi.fn(async()=>{}),dispose:vi.fn(async()=>{})}
        const pin=vi.fn(async()=>publication)
        const h=await createHarness({pin})
        const incoming={...makeDatabase(),username:'Upstream import'}
        await expect(h.runtime.replacePersistentDatabase(incoming,'upstream-import',{upstreamImport:true,publishOfficial})).resolves.toEqual({kind:'committed',revision:2,projection:'applied'})
        expect(h.store.replaceFromDatabase).toHaveBeenCalledWith(expect.objectContaining({username:'Upstream import',explicitGlobalChatVariables:{}}),1,[],undefined,
            {bindingAuthority:'9007199254740993',requestId:expect.any(String)})
        expect(binding.fence).toHaveBeenCalledOnce()
        expect(binding.resume).toHaveBeenCalledOnce()
        expect(h.database.username).toBe('Upstream import')
        expect(h.runtime.hasPendingOfficialPublication()).toBe(publishOfficial)
        if(publishOfficial){
            await h.runtime.publishCurrentOfficialRevision()
            expect(pin).toHaveBeenCalledExactlyOnceWith(2)
            expect(publication.publish).toHaveBeenCalledOnce()
        }else expect(pin).not.toHaveBeenCalled()
        await h.runtime.flushPendingDataLocally('import-baseline')
        expect(h.store.commit).not.toHaveBeenCalled()
    })
    it('clears stale pending official publication on a later upstream import without that option',async()=>{
        const pin=vi.fn(async()=>({publish:async()=>{},dispose:async()=>{}}))
        const h=await createHarness({pin})
        await h.runtime.replacePersistentDatabase({...makeDatabase(),username:'First'},'first',{upstreamImport:true,publishOfficial:true})
        expect(h.runtime.hasPendingOfficialPublication()).toBe(true)
        await h.runtime.replacePersistentDatabase({...makeDatabase(),username:'Second'},'second',{upstreamImport:true,publishOfficial:false})
        expect(h.runtime.hasPendingOfficialPublication()).toBe(false)
        expect(pin).not.toHaveBeenCalled()
        expect(h.database.username).toBe('Second')
    })
})


describe('native upstream job activation recovery with the real runtime',()=>{
    it('cancels and drains a proven busy finalize rejection before exact unchanged abort and resume',async()=>{
        const h=await createHarness()
        const original=structuredClone(h.database)
        let cancelled=false
        const invoke=vi.fn(async(command:string)=>{
            if(command==='native_file_job_start')return {jobId:'synthetic-busy'}
            if(command==='native_file_job_status')return {state:cancelled?'cancelled':'waitingForInput',phase:cancelled?'cancelled':'awaiting-activation',kind:'restore-block-risu-save'}
            if(command==='native_file_job_finalize')throw {code:'library-operation-busy',message:'library-operation-busy'}
            if(command==='native_file_job_cancel'){cancelled=true;return {outcome:'requested'}}
            if(command==='native_file_job_forget')return true
            throw new Error(`Unexpected native command ${command}`)
        })
        plugins.restartPluginsAfterAuthorityReplacement.mockImplementation(async()=>{
            expect(cancelled).toBe(true)
            expect(h.database).toEqual(original)
        })
        await expect(runNativeBlockRisuSaveRestore(h.runtime,{type:'desktopPath',path:'E:/synthetic/busy.risudat'}, {},
            {isTauri:()=>true,invoke,wait:async()=>{}})).rejects.toMatchObject({code:'library-operation-busy'})
        expect(h.database).toEqual(original)
        expect(h.runtime.revision).toBe(1)
        expect(()=>h.runtime.markPersistentDataDirty(1)).not.toThrow()
        expect(plugins.restartPluginsAfterAuthorityReplacement).toHaveBeenCalledOnce()
        expect(binding.resume).toHaveBeenCalledOnce()
        expect(invoke.mock.calls.filter(([command])=>command==='native_file_job_finalize')).toHaveLength(1)
        expect(invoke.mock.calls.filter(([command])=>command==='native_file_job_cancel')).toHaveLength(1)
        await expect(h.runtime.retryCommittedWorkingSetRefresh()).resolves.toBeNull()
    })
    it.each(['observer','finalize-response','status-response','late','inaccessible','restart','resume'] as const)('reconciles before strict adoption and resumes once: %s',async failure=>{
        const h=await createHarness()
        const incoming={...makeDatabase(),username:'Native staged import'}
        const result={revision:2,sourceBytes:16,sourceSha256:'a'.repeat(64),characterCount:1,presetCount:0,warningCodes:[]}
        let submitted=false
        let finished=false
        let unavailable=false
        const events:string[]=[]
        vi.mocked(h.store.commitWorkingSetChangeCursor!).mockImplementation(async revision=>{events.push(`cursor-${revision}`)})
        plugins.restartPluginsAfterAuthorityReplacement.mockImplementation(async()=>{
            expect(h.database.username).toBe(incoming.username)
            expect(()=>h.runtime.markPersistentDataDirty(1)).toThrow()
            expect(events).toContain('cursor-2')
            await expect(h.runtime.commitPersistentUnitIntent('plugin-initial-setter',[{type:'set',key:JSON.stringify(['root','username']),value:'Stale plugin setter'}])).rejects.toThrow()
            if(failure==='restart'&&plugins.restartPluginsAfterAuthorityReplacement.mock.calls.length===1)throw new Error('fresh restart failed')
            events.push('restart')
        })
        binding.resume.mockImplementation(async()=>{
            expect(events).toContain('restart')
            expect(()=>h.runtime.markPersistentDataDirty(1)).not.toThrow()
            if(failure==='resume'&&binding.resume.mock.calls.length===1)throw new Error('adapter resume failed')
            events.push('resume')
        })
        const invoke=vi.fn(async(command:string)=>{
            if(command==='native_file_job_start') return {jobId:'synthetic-recovery'}
            if(command==='native_file_job_status'){
                events.push('native-status')
                if(unavailable) throw new Error('status inaccessible')
                if(!submitted) return {state:'waitingForInput',phase:'awaiting-activation',kind:'restore-block-risu-save'} as NativeFileJobStatus
                if(failure==='status-response'&&!finished){finished=true;throw new Error('status response lost')}
                return {state:finished?'succeeded':'running',phase:finished?'complete':'activating-database',kind:'restore-block-risu-save',...(finished?{result}:{})} as NativeFileJobStatus
            }
            if(command==='native_file_job_finalize'){
                submitted=true
                if(failure!=='late'&&failure!=='inaccessible'){h.nativeCommit(incoming);finished=failure!=='status-response'}
                if(failure==='finalize-response'||failure==='late'||failure==='inaccessible'||failure==='restart'){
                    unavailable=failure==='inaccessible'
                    throw new Error('finalize response lost')
                }
                return {outcome:'requested'}
            }
            if(command==='native_file_job_forget') return true
            throw new Error(`Unexpected native command ${command}`)
        })
        const followup=vi.fn(async()=>{})
        await expect(runNativeBlockRisuSaveRestore(h.runtime,{type:'desktopPath',path:'E:/synthetic/upstream.risudat'},
            {afterRefresh:followup,onNativeStatus:async status=>{if((failure==='observer'||failure==='resume')&&status.state==='succeeded')throw new Error('committed observer failed')}},
            {isTauri:()=>true,invoke,wait:async()=>{}})).rejects.toThrow()
        expect(()=>h.runtime.markPersistentDataDirty(1)).toThrow()
        expect(binding.resume).not.toHaveBeenCalled()
        expect(invoke.mock.calls.filter(([command])=>command==='native_file_job_cancel')).toHaveLength(0)
        const projections=h.replaceDatabase.mock.calls.length
        if(failure==='late'||failure==='inaccessible'){
            await expect(h.runtime.retryCommittedWorkingSetRefresh()).resolves.toMatchObject({projection:'refresh-required'})
            expect(h.replaceDatabase).toHaveBeenCalledTimes(projections)
            expect(plugins.restartPluginsAfterAuthorityReplacement).not.toHaveBeenCalled()
            expect(binding.resume).not.toHaveBeenCalled()
            expect(()=>h.runtime.markPersistentDataDirty(1)).toThrow()
            if(failure==='inaccessible') return
            h.nativeCommit(incoming);finished=true
        }
        if(failure==='restart'||failure==='resume'){
            await expect(h.runtime.retryCommittedWorkingSetRefresh()).resolves.toMatchObject({projection:'refresh-required'})
            expect(()=>h.runtime.markPersistentDataDirty(1)).toThrow()
            expect(followup).not.toHaveBeenCalled()
        }
        await expect(h.runtime.retryCommittedWorkingSetRefresh()).resolves.toEqual({kind:'committed',revision:2,projection:'applied'})
        expect(h.database.username).toBe(incoming.username)
        expect(h.runtime.pendingWorkingSetRefreshRevision).toBeNull()
        expect(plugins.restartPluginsAfterAuthorityReplacement).toHaveBeenCalledTimes(failure==='restart'?2:1)
        expect(binding.resume).toHaveBeenCalledTimes(failure==='resume'?2:1)
        expect(followup).toHaveBeenCalledExactlyOnceWith(true)
        expect(invoke.mock.calls.filter(([command])=>command==='native_file_job_finalize')).toHaveLength(1)
        expect(events.indexOf('cursor-2')).toBeLessThan(events.indexOf('restart'))
        expect(events.indexOf('restart')).toBeLessThan(events.indexOf('resume'))
        expect(invoke.mock.calls.filter(([command])=>command==='native_file_job_forget')).toHaveLength(1)
        await expect(retryCommittedWorkingSetRefreshWithContinuation(h.runtime)).resolves.toBeNull()
    })
})
