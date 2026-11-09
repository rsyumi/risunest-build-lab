import {beforeEach, describe, expect, it, vi} from 'vitest'
import type {SyncBindingState, SyncBindingTransport} from './bindingFlow'
const native = vi.hoisted(()=>({state:vi.fn(),assertAuthority:vi.fn()}))
vi.mock('./bindingNative',()=>({createNativeSyncBindingBridge:()=>native}))
import {bindSyncTarget,isSyncBindingReplacementPending,prepareBoundLibraryReplacement,registerSyncBindingFlow,registerSyncBindingTransport,resumeCurrentSyncBinding,unbindSyncTarget} from './bindingRegistry'
import {subscribeSyncBindingChanges} from './bindingChanges'
import {createSyncBindingFlow} from './bindingFlow'
import {createPluginStartupGate} from '../../plugins/pluginStartupGate'
import {get} from 'svelte/store'
const state: SyncBindingState={target:{kind:'server',connectionId:'synthetic'},targetAuthority:'1',selectionEpoch:'2',libraryId:'synthetic-library',progress:null}
beforeEach(()=>{native.state.mockReset().mockResolvedValue(state);native.assertAuthority.mockReset().mockResolvedValue(undefined)})
function transport(receive=vi.fn(async()=>{})) {
    return {receiveAvailableChanges:receive,fenceOldJobs:vi.fn(async()=>{}),resumeBinding:vi.fn(async()=>{})} as unknown as SyncBindingTransport
}
describe('bound library replacement prerequisite',()=>{
    it('resumes the real binding flow after abandoned preparation rather than retaining its old resumed flag',async()=>{
        let active=false,starts=0
        const adapter=transport()
        adapter.resumeBinding=async()=>{active=true}
        adapter.fenceOldJobs=async()=>{active=false}
        adapter.receiveAvailableChanges=async()=>{if(!active)throw new Error('Sync binding changed')}
        const unsupported=async():Promise<never>=>{throw new Error('Unexpected binding operation')}
        const flow=createSyncBindingFlow({
            native:{state:native.state,assertAuthority:native.assertAuthority,switchTarget:unsupported},
            resolveTransport:()=>adapter,
            gate:{runTransition:operation=>operation(),runWrite:operation=>operation(),runKeyedWrite:(_key,operation)=>operation()},
            plugins:{fenceExecution:unsupported,invalidateCaches:unsupported,restart:unsupported},
            withPausedWrites:operation=>operation(),hasLocalCharacters:unsupported,hasNonDefaultSharedData:unsupported,
            confirmReplacement:unsupported,refreshActivatedLibrary:unsupported,beginActivatedLibraryGuard:()=>{throw new Error('Unexpected activation')},
        })
        const releaseFlow=registerSyncBindingFlow(flow)
        const releaseTransport=registerSyncBindingTransport(state.target as {kind:'server';connectionId:string},adapter)
        const gate=createPluginStartupGate({enabled:()=>true,delay:()=>true,replacementPending:()=>isSyncBindingReplacementPending(),
            context:async()=>({identity:'synthetic',bound:true,receive:async()=>{await adapter.receiveAvailableChanges!({state,signal:new AbortController().signal})},assertCurrent:async()=>{}}),
            start:async()=>{if(await gate.ensure())starts++},
        })
        const stop=subscribeSyncBindingChanges(()=>{if(gate.isWaiting())gate.retry()})
        try {
            await resumeCurrentSyncBinding(state.target)
            expect(active).toBe(true)
            const replacement=await prepareBoundLibraryReplacement()
            await replacement.fence()
            expect(active).toBe(false)
            expect(await gate.ensure()).toBe(false)
            replacement.release()
            expect(isSyncBindingReplacementPending()).toBe(false)
            await vi.waitFor(()=>expect(get(gate.state).phase).toBe('waiting'))
            await resumeCurrentSyncBinding(state.target)
            expect(active).toBe(true)
            await vi.waitFor(()=>expect(starts).toBe(1))
        } finally {stop();releaseFlow();releaseTransport()}
    })
    it.each(['held', 'receive', 'failed'] as const)('keeps replacement pending until %s resume settles and notifies afterward',async outcome=>{
        let finish!:()=>void
        let held = false
        const waiting = new Promise<void>(resolve=>{finish=resolve})
        const adapter = transport()
        adapter.resumeBinding = async context => {
            await waiting
            held = context.holdAutomaticSync === true
            if (outcome === 'failed') throw new Error('resume failed')
        }
        const release=registerSyncBindingTransport(state.target as {kind:'server';connectionId:string},adapter)
        const observations: {pending:boolean;held:boolean}[]=[]
        const stop=subscribeSyncBindingChanges(()=>observations.push({pending:isSyncBindingReplacementPending(state),held}))
        try {
            const prepared=await prepareBoundLibraryReplacement()
            await prepared.fence()
            expect(isSyncBindingReplacementPending(state)).toBe(true)
            expect(isSyncBindingReplacementPending({...state,targetAuthority:'other'})).toBe(false)
            const resumed=prepared.resume({holdAutomaticSync:outcome==='held'})
            const settled=outcome==='failed'?expect(resumed).rejects.toThrow('resume failed'):expect(resumed).resolves.toBeUndefined()
            await Promise.resolve()
            expect(observations).toEqual([])
            finish();await settled
            expect(observations).toEqual([{pending:false,held:outcome==='held'}])
        } finally {finish();stop();release()}
    })
    it('clears failed fences and unregistered transports without affecting another replacement',async()=>{
        const adapter=transport()
        const release=registerSyncBindingTransport(state.target as {kind:'server';connectionId:string},adapter)
        let starts=0
        const gate=createPluginStartupGate({enabled:()=>true,delay:()=>true,replacementPending:()=>isSyncBindingReplacementPending(),
            context:async()=>({identity:'synthetic',bound:true,receive:async()=>{},assertCurrent:async()=>{}}),
            start:async()=>{if(await gate.ensure())starts++},
        })
        const stop=subscribeSyncBindingChanges(()=>{if(gate.isWaiting())gate.retry()})
        try {
            const prepared=await prepareBoundLibraryReplacement()
            try {
                adapter.fenceOldJobs=async()=>{throw new Error('fence failed')}
                await expect(prepared.fence()).rejects.toThrow('fence failed')
                expect(isSyncBindingReplacementPending(state)).toBe(false)
                adapter.fenceOldJobs=async()=>{}
                await prepared.fence()
                expect(isSyncBindingReplacementPending(state)).toBe(true)
                expect(await gate.ensure()).toBe(false)
            } finally {release()}
            expect(isSyncBindingReplacementPending(state)).toBe(false)
            // Plugin startup that waited on the dropped replacement checks again, though the replacement then ends without resuming.
            prepared.release()
            await vi.waitFor(()=>expect(starts).toBe(1))
        } finally {stop()}
    })
    it('skips new catchup for an exact committed restore while retaining old-job and authority fences',async()=>{
        const adapter={fenceOldJobs:vi.fn(async()=>{}),resumeBinding:vi.fn(async()=>{})} as unknown as SyncBindingTransport
        const release=registerSyncBindingTransport(state.target as {kind:'server';connectionId:string},adapter)
        try {
            const prepared=await prepareBoundLibraryReplacement({confirmedCommittedRestore:true})
            await prepared.fence()
            expect(adapter.fenceOldJobs).toHaveBeenCalledOnce()
            expect(native.assertAuthority).toHaveBeenCalledTimes(2)
            native.state.mockResolvedValue({...state,libraryId:'synthetic-other-library'})
            await expect(prepared.assertAuthority()).rejects.toThrow('changed')
        } finally {release()}
    })
    it('fails closed when its bound adapter or receive hook is unavailable',async()=>{
        await expect(prepareBoundLibraryReplacement()).rejects.toThrow('unavailable')
        const release=registerSyncBindingTransport(state.target as {kind:'server';connectionId:string},{fenceOldJobs:async()=>{}} as unknown as SyncBindingTransport)
        try {await expect(prepareBoundLibraryReplacement()).rejects.toThrow('unavailable')} finally {release()}
    })
    it('reports an unavailable bound target as sync-unavailable, not a revision conflict',async()=>{
        await expect(prepareBoundLibraryReplacement()).rejects.toMatchObject({code:'sync-unavailable'})
        const adapter=transport(vi.fn(async()=>{throw new Error('Sync binding changed')}))
        const release=registerSyncBindingTransport(state.target as {kind:'server';connectionId:string},adapter)
        try {
            await expect(prepareBoundLibraryReplacement()).rejects.toMatchObject({code:'sync-unavailable'})
            expect(adapter.fenceOldJobs).not.toHaveBeenCalled()
        } finally {release()}
    })
    it('awaits available remote head before fencing and resuming its captured context',async()=>{
        let finish!:()=>void
        const received=new Promise<void>(resolve=>{finish=resolve})
        const adapter=transport(vi.fn(()=>received))
        const release=registerSyncBindingTransport(state.target as {kind:'server';connectionId:string},adapter)
        try {
            const preparation=prepareBoundLibraryReplacement()
            await Promise.resolve();await Promise.resolve()
            expect(adapter.fenceOldJobs).not.toHaveBeenCalled()
            finish()
            const prepared=await preparation
            expect(prepared.bound).toBe(true)
            await prepared.fence();await prepared.resume()
            expect(adapter.receiveAvailableChanges).toHaveBeenCalledWith(expect.objectContaining({state}))
            expect(adapter.fenceOldJobs).toHaveBeenCalledOnce()
            expect(adapter.resumeBinding).toHaveBeenCalledOnce()
        }finally{release()}
    })
    it.each(['library','target'])('rejects changed %s after bootstrap even with unchanged authority and epoch',async field=>{
        const adapter=transport(vi.fn(async()=>{native.state.mockResolvedValue({...state,...(field==='library'?{libraryId:'other'}:{target:{kind:'external',connectionId:'other'}})})}))
        const release=registerSyncBindingTransport(state.target as {kind:'server';connectionId:string},adapter)
        try{await expect(prepareBoundLibraryReplacement()).rejects.toThrow('changed')}finally{release()}
    })
    it('captures unbound authority so a new binding cannot pass unchanged abort proof',async()=>{
        native.state.mockResolvedValue({...state,target:{kind:'none'}})
        const prepared=await prepareBoundLibraryReplacement()
        expect(prepared.bound).toBe(false)
        native.state.mockResolvedValue(state)
        await expect(prepared.assertAuthority()).rejects.toThrow('changed')
    })
})
describe('sync target change notices',()=>{
    const target={kind:'external' as const,connectionId:'synthetic-connection'}
    function flow(bind:()=>Promise<unknown>,unbind:()=>Promise<unknown>=async()=>state) {
        return registerSyncBindingFlow({bind,unbind} as unknown as Parameters<typeof registerSyncBindingFlow>[0])
    }
    it('tells every view once the change has settled, whatever its outcome',async()=>{
        const seen=vi.fn()
        const stop=subscribeSyncBindingChanges(seen)
        const releaseTransport=registerSyncBindingTransport(target,transport())
        let finish!:(value:unknown)=>void
        const outcomes=[new Promise(resolve=>{finish=resolve}),Promise.resolve({kind:'cancelled'}),Promise.reject(new Error('inspect failed'))]
        const release=flow(()=>outcomes.shift()!)
        try {
            const pending=bindSyncTarget(target)
            await Promise.resolve();await Promise.resolve()
            expect(seen).not.toHaveBeenCalled()
            finish({kind:'bound'})
            await pending
            expect(seen).toHaveBeenCalledOnce()
            await bindSyncTarget(target)
            expect(seen).toHaveBeenCalledTimes(2)
            await expect(bindSyncTarget(target)).rejects.toThrow('inspect failed')
            expect(seen).toHaveBeenCalledTimes(3)
            await unbindSyncTarget()
            expect(seen).toHaveBeenCalledTimes(4)
            stop()
            await unbindSyncTarget()
            expect(seen).toHaveBeenCalledTimes(4)
        } finally {release();releaseTransport();stop()}
    })
})
