import { describe, expect, it, vi } from 'vitest'

import type { Chat, Database, Message } from './storage/database.svelte'
import { removeChatMessage } from './chatRemoval'

function deferred<T>() {
    let resolve!: (value: T) => void
    const promise = new Promise<T>((resolvePromise) => {
        resolve = resolvePromise
    })
    return { promise, resolve }
}

function currentTarget(id: string) {
    const messages: Message[] = [
        { role: 'user', data: `${id}-zero`, chatId: `${id}-zero` },
        { role: 'char', data: `${id}-one`, chatId: `${id}-one` },
    ]
    const conversation = {
        id: `${id}-chat`,
        message: messages,
    } as Chat
    const character = {
        type: 'character',
        chaId: `${id}-character`,
        chatPage: 0,
        chats: [conversation],
    } as Database['characters'][number]
    return { character, conversation }
}

describe('removeChatMessage', () => {
    it('does not fall back to the absolute index when a viewport target is unavailable', async () => {
        const current = currentTarget('current')

        await expect(removeChatMessage({
            absoluteIndex: 0,
            captureTarget: () => null,
            shiftKey: false,
            recursive: false,
            askRemoval: false,
            instantRemove: false,
            captureCurrent: () => current,
            getCurrentSession: () => null,
            confirmRemoval: vi.fn(async () => true),
            confirmInstantRemoval: vi.fn(async () => ({ confirmed: true, checked: true })),
        })).resolves.toBe('stale')

        expect(current.conversation.message).toHaveLength(2)
    })

    it('aborts a no-session delete when navigation changes during confirmation', async () => {
        const original = currentTarget('original')
        const replacement = currentTarget('replacement')
        let current = original
        const confirmation = deferred<boolean>()
        const confirmInstantRemoval = vi.fn(async () => ({ confirmed: true, checked: true }))

        const removing = removeChatMessage({
            absoluteIndex: 1,
            shiftKey: false,
            recursive: false,
            askRemoval: true,
            instantRemove: false,
            captureCurrent: () => current,
            getCurrentSession: () => null,
            confirmRemoval: () => confirmation.promise,
            confirmInstantRemoval,
        })
        current = replacement
        confirmation.resolve(true)

        await expect(removing).resolves.toBe('stale')
        expect(original.conversation.message.map((message) => message.data)).toEqual([
            'original-zero',
            'original-one',
        ])
        expect(replacement.conversation.message.map((message) => message.data)).toEqual([
            'replacement-zero',
            'replacement-one',
        ])
        expect(confirmInstantRemoval).not.toHaveBeenCalled()
    })

    it('aborts a no-session truncate when navigation changes during the option dialog', async () => {
        const original = currentTarget('original')
        const replacement = currentTarget('replacement')
        let current = original
        const confirmation = deferred<{ confirmed: boolean; checked: boolean }>()

        const removing = removeChatMessage({
            absoluteIndex: 1,
            shiftKey: false,
            recursive: false,
            askRemoval: false,
            instantRemove: true,
            captureCurrent: () => current,
            getCurrentSession: () => null,
            confirmRemoval: vi.fn(async () => true),
            confirmInstantRemoval: () => confirmation.promise,
        })
        current = replacement
        confirmation.resolve({ confirmed: true, checked: true })

        await expect(removing).resolves.toBe('stale')
        expect(original.conversation.message.map((message) => message.data)).toEqual([
            'original-zero',
            'original-one',
        ])
        expect(replacement.conversation.message.map((message) => message.data)).toEqual([
            'replacement-zero',
            'replacement-one',
        ])
    })
    it.each([true, false])('uses one option dialog and deletes the later messages only when checked: %s', async checked => {
        const current = currentTarget('current')
        const confirmRemoval = vi.fn(async () => true)
        const confirmInstantRemoval = vi.fn(async () => ({ confirmed: true, checked }))
        expect(await removeChatMessage({ absoluteIndex: 0, shiftKey: false, recursive: true, askRemoval: true, instantRemove: false,
            captureCurrent: () => current, getCurrentSession: () => null, confirmRemoval, confirmInstantRemoval })).toBe('removed')
        expect(confirmRemoval).not.toHaveBeenCalled()
        expect(confirmInstantRemoval).toHaveBeenCalledOnce()
        expect(current.conversation.message.map(message => message.chatId)).toEqual(checked ? [] : ['current-one'])
    })
    it('cancels the option dialog without deleting or truncating', async () => {
        const current = currentTarget('current')
        expect(await removeChatMessage({ absoluteIndex: 0, shiftKey: false, recursive: true, askRemoval: true, instantRemove: false,
            captureCurrent: () => current, getCurrentSession: () => null, confirmRemoval: vi.fn(async () => true),
            confirmInstantRemoval: async () => ({ confirmed: false, checked: false }) })).toBe('cancelled')
        expect(current.conversation.message).toHaveLength(2)
    })

})
