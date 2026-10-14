import { expect, it, vi } from 'vitest'
import type { Chat, Database } from '../../storage/database.svelte'
import { ActiveConversationSession } from '../../storage/activeConversationSession'

const live = vi.hoisted(() => ({
    database: {} as Database,
    session: null as ActiveConversationSession | null,
}))

vi.mock('../../storage/database.svelte', () => ({
    getDatabase: () => live.database,
    getCurrentChat: () => live.database.characters[0].chats[0],
}))
vi.mock('../../storage/persistentDataRuntime.svelte', async (importOriginal) => ({
    ...await importOriginal<typeof import('../../storage/persistentDataRuntime.svelte')>(),
    peekActiveConversationSession: () => live.session,
}))
vi.mock('../../stores.svelte', async () => {
    const { writable } = await import('svelte/store')
    return {
        DBState: { get db() { return live.database } },
        selIdState: { selId: 0 },
        selectedCharID: writable(0),
        ReloadChatPointer: writable({}),
        ScriptDisplayRefreshPointer: writable(0),
    }
})
vi.mock('../memory/hypamemory', () => ({ HypaProcesser: class {} }))

import { GraphMemClient } from './graphmem'

it('commits a written memory through the session that owns the selected conversation', async () => {
    const conversation = {
        id: 'conversation',
        message: [],
        scriptstate: { $graphmem_graph: '[]' },
    } as unknown as Chat
    live.database = {
        characters: [{ chaId: 'character', chatPage: 0, chats: [conversation], defaultVariables: '' }],
        templateDefaultVariables: '',
    } as unknown as Database
    const session = new ActiveConversationSession({
        characterId: 'character',
        conversationId: 'conversation',
        conversation,
        storeRevision: 0,
    })
    live.session = session

    await expect(new GraphMemClient().callTool('writeMemory', { name: 'place', summary: 'synthetic' }))
        .resolves.toEqual([{ type: 'text', text: 'Memory entry "place" written successfully.' }])

    expect(session.version).toBe(1)
    expect(JSON.parse(session.readChatVariable('graphmem_graph') as string)).toEqual([
        { name: 'place', summary: 'synthetic', connections: [] },
    ])
})
