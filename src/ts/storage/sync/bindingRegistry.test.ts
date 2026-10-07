import {beforeEach, describe, expect, it, vi} from 'vitest'
import type {SyncBindingState, SyncBindingTransport} from './bindingFlow'
const native = vi.hoisted(()=>({state:vi.fn(),assertAuthority:vi.fn()}))
vi.mock('./bindingNative',()=>({createNativeSyncBindingBridge:()=>native}))
import {bindSyncTarget,prepareBoundLibraryReplacement,registerSyncBindingFlow,registerSyncBindingTransport,unbindSyncTarget} from './bindingRegistry'
import {subscribeSyncBindingChanges} from './bindingChanges'
const state: SyncBindingState={target:{kind:'server',connectionId:'synthetic'},targetAuthority:'1',selectionEpoch:'2',libraryId:'synthetic-library',progress:null}
beforeEach(()=>{native.state.mockReset().mockResolvedValue(state);native.assertAuthority.mockReset().mockResolvedValue(undefined)})
function transport(receive=vi.fn(async()=>{})) {
    return {receiveAvailableChanges:receive,fenceOldJobs:vi.fn(async()=>{}),resumeBinding:vi.fn(async()=>{})} as unknown as SyncBindingTransport
}
describe('bound library replacement prerequisite',()=>{
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
