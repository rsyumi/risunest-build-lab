import { describe, expect, it } from 'vitest'
import type { Message, character } from './database.svelte'
import type { ConversationMutation, WorkingSetCommit } from './persistentDataStore'
import { planConversationInsertPages } from './conversationInsertPages'

const message = (data: string): Message => ({ role: 'user', data }) as Message
const bytes = (value: Message) => JSON.stringify(value).length + 1
const create = (conversationId: string, messages: Message[], configuredIndex: number): ConversationMutation => ({
    type: 'replace-range', characterId: 'char', conversationId, start: 0, deleteCount: 0, messages,
    conversation: { id: conversationId, name: conversationId } as never, configuredIndex,
})

describe('planConversationInsertPages', () => {
    it('leaves a save that creates no conversation to fail as it is', () => {
        expect(planConversationInsertPages({
            expectedRevision: 4,
            conversations: [{ type: 'replace-range', characterId: 'char', conversationId: 'old', start: 0, deleteCount: 1, messages: [message('edit')] }],
        })).toBeNull()
    })

    it('pages created conversations in order and moves the reorder and detail to the last commit', () => {
        const [a, b, c] = [message('a'.repeat(40)), message('b'.repeat(40)), message('c'.repeat(200))]
        const small = message('s')
        const budget = bytes(a) + bytes(b)
        const commit: WorkingSetCommit = {
            expectedRevision: 4,
            unitMutations: [{ type: 'set', key: '["root","username"]', value: 'synthetic' }],
            character: { type: 'character', chaId: 'char', name: 'Synthetic', chatPage: 2 } as never,
            conversations: [
                { type: 'delete', characterId: 'char', conversationId: 'gone' },
                create('large', [a, b, c, a], 1),
                create('small', [small], 2),
                { type: 'reorder', characterId: 'char', conversationIds: ['kept', 'large', 'small'] },
            ],
        }
        const plan = planConversationInsertPages(commit, budget)!
        expect(plan.steps).toEqual([
            {
                unitMutations: commit.unitMutations,
                conversations: [
                    { type: 'delete', characterId: 'char', conversationId: 'gone' },
                    { ...commit.conversations![1], messages: [a, b] },
                ],
            },
            { conversations: [{ type: 'replace-range', characterId: 'char', conversationId: 'large', start: 2, deleteCount: 0, messages: [c] }] },
            {
                character: commit.character,
                conversations: [
                    { type: 'replace-range', characterId: 'char', conversationId: 'large', start: 3, deleteCount: 0, messages: [a] },
                    commit.conversations![2],
                    commit.conversations![3],
                ],
            },
        ])
    })

    it('applies the conversation order of a character that gains a conversation in the last commit', () => {
        const [a, b] = [message('a'.repeat(40)), message('b'.repeat(40))]
        const gainingOrder = { type: 'set' as const, key: '["order","conversations","char"]', value: { ids: ['one', 'kept', 'two'], folders: [] } }
        const otherOrder = { type: 'set' as const, key: '["order","conversations","other"]', value: { ids: ['x'], folders: [] } }
        const plan = planConversationInsertPages({
            expectedRevision: 1,
            unitMutations: [gainingOrder, otherOrder],
            conversations: [create('one', [a], 0), create('two', [b], 2)],
        }, bytes(a))!
        expect(plan.steps).toEqual([
            { unitMutations: [otherOrder], conversations: [create('one', [a], 0)] },
            { unitMutations: [gainingOrder], conversations: [create('two', [b], 2)] },
        ])
    })

    it('starts a new commit when the next created conversation does not fit', () => {
        const [a, b] = [message('a'.repeat(40)), message('b'.repeat(40))]
        const plan = planConversationInsertPages({
            expectedRevision: 1,
            conversations: [create('one', [a], 0), create('two', [b], 1)],
        }, bytes(a))!
        expect(plan.steps.map((step) => step.conversations.map((mutation) => 'conversationId' in mutation ? mutation.conversationId : ''))).toEqual([['one'], ['two']])
    })

    it('creates an added character with its first bounded page and keeps later pages out of the character envelope', () => {
        const messages = [message('a'.repeat(40)), message('b'.repeat(40))]
        const added = { type: 'character', chaId: 'char', name: 'Synthetic', chatPage: 0,
            chats: [{ id: 'new', name: 'New', message: messages }] } as unknown as character
        const plan = planConversationInsertPages({ expectedRevision: 1, addCharacter: added }, bytes(messages[0]))!
        expect(plan.addedCharacterId).toBe('char')
        expect(plan.steps).toEqual([
            { addCharacter: { ...added, chats: [] }, conversations: [{ ...create('new', [messages[0]], 0), conversation: { id: 'new', name: 'New' } }] },
            { conversations: [{ type: 'replace-range', characterId: 'char', conversationId: 'new', start: 1, deleteCount: 0, messages: [messages[1]] }] },
        ])
    })
})
