import { describe, expect, it, vi } from 'vitest'

import type { Chat, Message } from '../storage/database.svelte'
import { ActiveConversationSession, ConversationSessionStaleError, MessageLocatorMismatchError } from '../storage/activeConversationSession'
import { beginPinnedConversationHistoryOperation } from '../storage/conversationHistoryOperation'
import {
    adoptTriggeredChat,
    createLivePromptHistoryCompatibilitySnapshot,
    ensurePromptHistoryEntryId,
    iteratePromptHistoryTargets,
    selectPromptHistory,
} from './promptHistory'

function message(
    data: string,
    role: Message['role'] = 'user',
    disabled: Message['disabled'] = false,
): Message {
    return { role, data, disabled, chatId: `message-${data}` }
}

function sessionFor(messages: Message[]) {
    const conversation: Chat = {
        id: 'conversation-a',
        name: 'Conversation A',
        note: '',
        localLore: [],
        message: messages,
    }
    return new ActiveConversationSession({
        characterId: 'character-a',
        conversationId: 'conversation-a',
        conversation,
        storeRevision: 23,
    })
}

describe('prompt history paging', () => {
    it('preserves the latest allBefore boundary, disabled filtering, order, duplicates, and roles', () => {
        const session = sessionFor([
            message('old-user'),
            message('reset', 'char', 'allBefore'),
            message('duplicate', 'user'),
            message('hidden', 'char', true),
            message('duplicate', 'char'),
            message('tail', 'user'),
        ])
        const operation = beginPinnedConversationHistoryOperation(session)

        const selection = selectPromptHistory(operation, 2)
        const entries = [...iteratePromptHistoryTargets(operation, selection, 2)]

        expect(selection).toEqual({
            startIndex: 2,
            endIndex: 6,
            totalMessages: 6,
            messageCount: 3,
            resetByAllBefore: true,
        })
        expect(entries.map((entry) => {
            const value = ensurePromptHistoryEntryId(operation, entry, () => 'unused')
            return { absoluteIndex: entry.absoluteIndex, relativeIndex: entry.relativeIndex, data: value.data, role: value.role }
        })).toEqual([
            { absoluteIndex: 2, relativeIndex: 0, data: 'duplicate', role: 'user' },
            { absoluteIndex: 4, relativeIndex: 1, data: 'duplicate', role: 'char' },
            { absoluteIndex: 5, relativeIndex: 2, data: 'tail', role: 'user' },
        ])
        operation.dispose()
    })

    it('uses bounded backward and forward reads for the Roadmap 14 corpus conversation', async () => {
        const { roadmap14Corpus } = await import('../storage/tests/roadmap14/losslessCorpus')
        const fixture = roadmap14Corpus.database.characters[0].chats[0].message
        const messages = Array.from({ length: 11 }, (_, index) => ({
            ...structuredClone(fixture[index % fixture.length]),
            chatId: `fixture-${index}`,
        }))
        const session = sessionFor(messages)
        const backward = vi.spyOn(session, 'scanMetadataBackward')
        const range = vi.spyOn(session, 'readMetadataRange')
        const operation = beginPinnedConversationHistoryOperation(session)

        const selection = selectPromptHistory(operation, 3)
        const entries = [...iteratePromptHistoryTargets(operation, selection, 3)]

        expect(entries.map((entry) => ensurePromptHistoryEntryId(operation, entry, () => 'unused'))).toEqual(messages)
        expect(backward.mock.calls.every(([, limit]) => limit <= 3)).toBe(true)
        expect(range.mock.calls.every(([, limit]) => limit <= 3)).toBe(true)
        expect(backward.mock.calls.length).toBeGreaterThan(1)
        expect(range.mock.calls.length).toBeGreaterThan(1)
        operation.dispose()
    })

    it('fails instead of mixing revisions between forward pages', () => {
        const session = sessionFor([
            message('zero'),
            message('one'),
            message('two'),
        ])
        const operation = beginPinnedConversationHistoryOperation(session)
        const selection = selectPromptHistory(operation, 1)
        const iterator = iteratePromptHistoryTargets(operation, selection, 1)

        expect(ensurePromptHistoryEntryId(operation, iterator.next().value!, () => 'unused').data).toBe('zero')
        session.append(message('new-tail'))

        expect(() => iterator.next()).toThrow(ConversationSessionStaleError)
        operation.dispose()
    })

    it('binds each paged entry to the latest live message before processing it', () => {
        const messages = [message('first'), message('before-update')]
        const session = sessionFor(messages)
        const operation = beginPinnedConversationHistoryOperation(session)
        const selection = selectPromptHistory(operation)
        const iterator = iteratePromptHistoryTargets(operation, selection)

        const firstEntry = iterator.next().value!
        const first = ensurePromptHistoryEntryId(operation, firstEntry, vi.fn())
        expect(first).toBe(messages[0])
        expect(first.data).toBe('first')
        messages[1].data = 'after-update'
        const secondEntry = iterator.next().value!
        const second = ensurePromptHistoryEntryId(operation, secondEntry, vi.fn())

        expect(second).toBe(messages[1])
        expect(second.data).toBe('after-update')
        operation.dispose()
    })

    it('selects and locates prompt messages without reading their bodies or extension payloads', () => {
        const messages = [
            message('old'),
            message('reset', 'char', 'allBefore'),
            message('hidden', 'char', true),
            message('selected'),
        ]
        const session = sessionFor(messages)
        const reads = vi.fn(() => { throw new Error('Message payload was read') })
        for (const value of messages) {
            Object.defineProperty(value, 'data', { enumerable: true, get: reads })
            Object.defineProperty(value, 'extensionPayload', { enumerable: true, get: reads })
        }
        const operation = beginPinnedConversationHistoryOperation(session)
        const selection = selectPromptHistory(operation, 2)
        const targets = [...iteratePromptHistoryTargets(operation, selection, 2)]

        expect(selection).toEqual({
            startIndex: 2, endIndex: 4, totalMessages: 4, messageCount: 1, resetByAllBefore: true,
        })
        expect(targets.map(({ absoluteIndex, relativeIndex }) => [absoluteIndex, relativeIndex]))
            .toEqual([[3, 0]])
        expect(ensurePromptHistoryEntryId(operation, targets[0], () => 'unused')).toBe(messages[3])
        expect(reads).not.toHaveBeenCalled()
        operation.dispose()
    })

    it.each([true, 'allBefore'] as const)('snapshots disabled=%s per target page and resolves bodies after an awaited script', async (disabled) => {
        const messages = [message('first'), message('second'), message('third'), message('fourth')]
        const session = sessionFor(messages)
        const operation = beginPinnedConversationHistoryOperation(session)
        const iterator = iteratePromptHistoryTargets(operation, selectPromptHistory(operation), 2)

        expect(iterator.next().value?.absoluteIndex).toBe(0)
        await Promise.resolve()
        messages[1].disabled = disabled
        messages[1].data = 'updated-second'
        messages[2].disabled = disabled
        const second = iterator.next().value!
        expect([second.absoluteIndex, second.relativeIndex]).toEqual([1, 1])
        expect(ensurePromptHistoryEntryId(operation, second, () => 'unused').data).toBe('updated-second')
        expect(iterator.next().value?.absoluteIndex).toBe(3)
        expect(iterator.next().done).toBe(true)
        operation.dispose()
    })

    it('rejects stale target pages and a stale completed traversal', () => {
        for (const pageSize of [1, 2]) {
            const session = sessionFor([message('first'), message('second')])
            const operation = beginPinnedConversationHistoryOperation(session)
            const iterator = iteratePromptHistoryTargets(operation, selectPromptHistory(operation), pageSize)
            iterator.next()
            if (pageSize === 2) iterator.next()
            session.append(message('new-tail'))

            expect(() => iterator.next()).toThrow(ConversationSessionStaleError)
            operation.dispose()
        }
    })

    it('reselects changed boundaries after a trigger and rejects a replaced target identity', () => {
        const messages = [message('first'), message('second'), message('third')]
        const session = sessionFor(messages)
        const operation = beginPinnedConversationHistoryOperation(session)
        expect(selectPromptHistory(operation).messageCount).toBe(3)
        messages[1].disabled = 'allBefore'
        const selection = selectPromptHistory(operation)
        expect(selection).toMatchObject({ startIndex: 2, messageCount: 1, resetByAllBefore: true })
        const target = iteratePromptHistoryTargets(operation, selection).next().value!
        messages[2] = message('third')

        expect(() => ensurePromptHistoryEntryId(operation, target, () => 'unused'))
            .toThrow(MessageLocatorMismatchError)
        operation.dispose()
    })

    it('preserves existing IDs and assigns IDs only to selected active prompt messages', () => {
        const messages = [
            { role: 'user', data: 'before-reset' },
            { role: 'char', data: 'reset', disabled: 'allBefore' },
            { role: 'user', data: 'disabled', disabled: true },
            { role: 'char', data: 'empty-id', chatId: '' },
            { role: 'user', data: 'existing', chatId: 'existing-id' },
        ] satisfies Message[]
        const session = sessionFor(messages)
        const operation = beginPinnedConversationHistoryOperation(session)
        const selection = selectPromptHistory(operation)
        const generated = ['generated-empty']

        for (const entry of iteratePromptHistoryTargets(operation, selection)) {
            ensurePromptHistoryEntryId(operation, entry, () => generated.shift()!)
        }

        expect(messages.map((value) => value.chatId)).toEqual([
            undefined,
            undefined,
            undefined,
            'generated-empty',
            'existing-id',
        ])
        expect(generated).toEqual([])
        operation.dispose()

        const repeatedOperation = beginPinnedConversationHistoryOperation(session)
        const repeatedSelection = selectPromptHistory(repeatedOperation)
        const generateAgain = vi.fn(() => 'different-id')
        for (const entry of iteratePromptHistoryTargets(repeatedOperation, repeatedSelection)) {
            ensurePromptHistoryEntryId(repeatedOperation, entry, generateAgain)
        }

        expect(generateAgain).not.toHaveBeenCalled()
        expect(messages[3].chatId).toBe('generated-empty')
        repeatedOperation.dispose()
    })

    it('adopts trigger results without replacing the active Chat identity', () => {
        const target: Chat = {
            id: 'conversation-a',
            name: 'Before',
            note: 'remove-me',
            localLore: [],
            message: [message('before')],
            folderId: 'removed-by-trigger',
        }
        const replacement: Chat = {
            id: 'conversation-a',
            name: 'After',
            note: '',
            localLore: [],
            message: [message('triggered')],
            scriptstate: { triggered: true },
        }
        const session = new ActiveConversationSession({
            characterId: 'character-a',
            conversationId: 'conversation-a',
            conversation: target,
            storeRevision: 12,
        })

        const adopted = adoptTriggeredChat(
            session,
            session.version,
            target.message,
            replacement,
        )

        expect(adopted).toBe(target)
        expect(adopted).toEqual(replacement)
        expect(session.matchesConversation('character-a', adopted)).toBe(true)
        expect(session.version).toBe(1)
        const operation = beginPinnedConversationHistoryOperation(session)
        expect(operation.readLatest(1).messages[0].data).toBe('triggered')
        operation.dispose()
    })

    it('retains and disposes the exact selected message references for compatibility', () => {
        const messages = [message('first'), message('retained'), message('tail')]
        const session = sessionFor(messages)
        const operation = beginPinnedConversationHistoryOperation(session)
        const selection = selectPromptHistory(operation)
        const retained = messages[1]
        const snapshot = createLivePromptHistoryCompatibilitySnapshot(messages, selection)

        messages.splice(1, 1, message('replacement'))
        retained.data = 'mutated-through-retained-reference'

        expect(snapshot.entries.map((entry) => entry.message.data)).toEqual([
            'first',
            'mutated-through-retained-reference',
            'tail',
        ])
        snapshot.dispose()
        expect(snapshot.entries).toEqual([])
        operation.dispose()
    })
})
