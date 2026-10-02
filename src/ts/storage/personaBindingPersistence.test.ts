import { expect, it, vi } from 'vitest'
import { captureRoot, deferred, makeDatabase, SaveCoordinator } from './saveCoordinator.testSupport'
import { applyConversationBindingPatch, type ConversationBindingPatch } from './conversationBinding'
import type { PersistentDataStore } from './persistentDataStore'
import { createConversationSummaryStub } from './conversationResidency'

function setup() {
    const database = makeDatabase()
    const character = database.characters[0]
    const other = createConversationSummaryStub({
        id: 'other',
        characterId: character.chaId,
        name: 'Synthetic',
        configuredIndex: 1,
        recentAt: 0,
        messageCount: 10_000,
    })
    character.chats.push(other)
    const readConversation = vi.fn(() => {
        throw new Error('Binding must not read message bodies')
    })
    let revision = 1
    const conversation: Record<string, unknown> = { id: 'other', name: 'Synthetic', note: '', localLore: [] }
    const commit = vi.fn(async (input: import('./persistentDataStore').WorkingSetCommit) => {
        for (const mutation of input.unitMutations ?? []) {
            const [kind, , , field] = JSON.parse(mutation.key)
            if (kind !== 'conversation') continue
            if (mutation.type === 'delete') delete conversation[field]
            else conversation[field] = structuredClone(mutation.value)
        }
        return { revision: ++revision }
    })
    const store = {
        readConversation,
        commit,
        readConversationMetadata: vi.fn(async () => ({
            revision,
            value: {
                characterId: character.chaId,
                conversationId: 'other',
                totalMessages: 10_000,
                conversation: { ...conversation },
            },
        })),
    } as unknown as PersistentDataStore
    const coordinator = new SaveCoordinator({
        store,
        captureRoot: () => captureRoot(database),
        captureSelectedCharacter: () => character,
        replaceDatabase: () => undefined,
    })
    coordinator.initialize(1)
    return { character, other, readConversation, commit, coordinator, store }
}

it('freezes binding values and explicit deletions through reads, commit and publication', async () => {
    const { character, other, readConversation, commit, coordinator, store } = setup()
    const readStarted = deferred<void>()
    const readFinished = deferred<void>()
    vi.mocked(store.readConversationMetadata).mockImplementationOnce(async () => {
        readStarted.resolve()
        await readFinished.promise
        return {
            revision: 1,
            value: {
                characterId: character.chaId, conversationId: 'other', totalMessages: 10_000,
                conversation: {
                    id: 'other', name: 'Synthetic', note: '', localLore: [], bindedPersona: 'old',
                    savedToggleValues: { old: 'value' },
                },
            },
        }
    })
    const commitStarted = deferred<void>()
    const commitFinished = deferred<{ revision: number }>()
    commit.mockImplementationOnce(() => {
        commitStarted.resolve()
        return commitFinished.promise
    })
    const patch: ConversationBindingPatch = {
        bindedPersona: undefined,
        savedToggleValues: { toggle_a: 'captured' },
    }
    const captured = structuredClone(patch)
    const publish = vi.fn((committedPatch: ConversationBindingPatch) => {
        if (committedPatch) applyConversationBindingPatch(other, committedPatch)
    })
    const binding = coordinator.mutateConversationBinding(character.chaId, 'other', patch, publish)
    patch.bindedPersona = 'Changed while queued'
    await readStarted.promise
    patch.savedToggleValues!.toggle_a = 'Changed during read'
    readFinished.resolve()
    await commitStarted.promise
    patch.savedToggleValues!.extra = 'Changed during commit'
    commitFinished.resolve({ revision: 2 })
    await binding

    const written = vi.mocked(store.commit).mock.calls[0][0].unitMutations!
    expect(written).toEqual([
        { key: JSON.stringify(['conversation', character.chaId, 'other', 'bindedPersona']), type: 'delete' },
        { key: JSON.stringify(['conversation', character.chaId, 'other', 'savedToggleValues']), type: 'set', value: captured.savedToggleValues },
    ])
    expect(publish).toHaveBeenCalledExactlyOnceWith(captured)
    expect(other.savedToggleValues).toEqual({ toggle_a: 'captured' })
    expect(other).not.toHaveProperty('bindedPersona')
    await coordinator.flushPendingDataLocally('clean-frozen-binding')
    expect(commit).toHaveBeenCalledOnce()
    expect(readConversation).not.toHaveBeenCalled()
    other.savedToggleValues!.toggle_a = 'Later UI edit'
    expect(written[1]).toMatchObject({ value: { toggle_a: 'captured' } })
})

it('writes an inactive persona binding using only metadata and advances the matching baseline', async () => {
    const { character, other, readConversation, commit, coordinator } = setup()
    await coordinator.mutateConversationBinding(character.chaId, 'other', { bindedPersona: 'persona' }, () => {
        other.bindedPersona = 'persona'
    })
    await coordinator.flushPendingData('verify-clean-binding')
    expect(readConversation).not.toHaveBeenCalled()
    expect(commit).toHaveBeenCalledTimes(1)
    expect(commit.mock.calls[0][0].unitMutations).toEqual([
        { key: JSON.stringify(['conversation', character.chaId, 'other', 'bindedPersona']), type: 'set', value: 'persona' },
    ])
    expect(commit.mock.calls[0][0]).not.toHaveProperty('conversations')
})

it('stores toggle bindings as metadata and drops the field again when the binding is removed', async () => {
    const { character, other, readConversation, commit, coordinator } = setup()
    const values = { toggle_a: '1' }
    await coordinator.mutateConversationBinding(character.chaId, 'other', { savedToggleValues: values }, () => {
        other.savedToggleValues = values
    })
    await coordinator.mutateConversationBinding(
        character.chaId,
        'other',
        { savedToggleValues: undefined },
        () => {
            delete other.savedToggleValues
        },
    )
    await coordinator.flushPendingData('verify-clean-binding')
    expect(readConversation).not.toHaveBeenCalled()
    expect(commit).toHaveBeenCalledTimes(2)
    const committed = commit.mock.calls.map((call) => call[0].unitMutations)
    expect(committed[0]).toEqual([{ key: JSON.stringify(['conversation', character.chaId, 'other', 'savedToggleValues']), type: 'set', value: { toggle_a: '1' } }])
    expect(committed[1]).toEqual([{ key: JSON.stringify(['conversation', character.chaId, 'other', 'savedToggleValues']), type: 'delete' }])
    expect(other).not.toHaveProperty('savedToggleValues')
})
