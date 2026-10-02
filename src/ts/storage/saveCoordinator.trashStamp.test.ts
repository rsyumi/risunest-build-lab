import {describe, expect, it, vi} from 'vitest'
vi.mock('../platform', () => ({isTauri:true}))
import {SaveCoordinator} from './saveCoordinator'
import type {PersistentDataStore, PersistentRevisionLease} from './persistentDataStore'
import {RevisionConflictError} from './persistentDataStore'
import {makeDatabase} from './saveCoordinator.testSupport'
import {capturePersistentRoot} from './persistentDataRuntime'

const day=24*60*60*1000
function fixture(stamp: string | undefined, recheck=stamp) {
    const database=makeDatabase()
    database.characterOrder=['char-a']
    database.loadouts=[{id:'loadout',characterIds:['char-a','char-b']}] as typeof database.loadouts
    const summary={id:'char-a',name:'Synthetic',type:'character' as const,configuredIndex:0,recentAt:0,trashed:true,conversationCount:0,trashTime:1,trashStampMs:stamp}
    const commit=vi.fn(async()=>({revision:2}))
    const lease={revision:1,readRoot:async()=>({revision:1,value:capturePersistentRoot(database)}),
        readCharacter:async()=>({revision:1,value:{...database.characters[0],trashTime:1}}),
        readCharacterSummary:async()=>({...summary,trashStampMs:recheck}),
        queryCharacters:async()=>({revision:1,items:[]}),release:vi.fn(async()=>{}),
    } as unknown as PersistentRevisionLease
    const store={queryCharacters:async()=>({revision:1,items:[summary]}),acquireRevision:async()=>lease,commit} as unknown as PersistentDataStore
    const coordinator=new SaveCoordinator({store,captureRoot:()=>capturePersistentRoot(database),captureCharacter:()=>null,captureSelectedCharacter:()=>null,replaceDatabase:()=>{}})
    coordinator.initialize(1)
    return {coordinator,commit,lease}
}

describe('native stamped trash expiry', () => {
    it.each([undefined,String(9*day)])('ignores missing or recent native stamps despite old mutable trashTime: %s', async stamp=>{
        const f=fixture(stamp)
        await expect(f.coordinator.expirePersistentTrash(10*day)).resolves.toBe(0)
        expect(f.commit).not.toHaveBeenCalled()
    })
    it('rechecks the latest pinned stamp before deleting a candidate', async()=>{
        const f=fixture('1',String(9*day))
        await expect(f.coordinator.expirePersistentTrash(10*day)).resolves.toBe(0)
        expect(f.commit).not.toHaveBeenCalled()
        expect(f.lease.release).toHaveBeenCalledOnce()
    })
    it('retires an expired ID and edits only changed order and loadout units', async()=>{
        const f=fixture('1')
        await expect(f.coordinator.expirePersistentTrash(10*day)).resolves.toBe(1)
        expect(f.commit).toHaveBeenCalledExactlyOnceWith({expectedRevision:1,unitMutations:[
            {type:'delete',key:'["exists","character","char-a"]'},
            {type:'set',key:'["order","characters"]',value:[]},
            {type:'set',key:'["record","loadouts","loadout"]',value:{id:'loadout',characterIds:['char-b']}},
        ]})
    })
    it('does not retry an expired candidate against a newer unchecked revision',async()=>{
        const f=fixture('1')
        f.commit.mockRejectedValueOnce(new RevisionConflictError(1,2))
        await expect(f.coordinator.expirePersistentTrash(10*day)).rejects.toBeInstanceOf(RevisionConflictError)
        expect(f.commit).toHaveBeenCalledOnce()
    })
})
