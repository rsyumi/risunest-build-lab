import { afterEach, beforeEach, expect, it, vi } from 'vitest'
import { mount, tick, unmount } from 'svelte'
import { languageEnglish } from '../../lang/en'
import { DBState } from 'src/ts/stores.svelte'
import { authorNoteFingerprint, effectiveAuthorNote } from 'src/ts/authorNotePresets'
import { applyConversationBindingPatch } from 'src/ts/storage/conversationBinding'
import type { AuthorNotePresetUsage } from 'src/ts/storage/persistentDataStore'
import AuthorNotePresetPopup from './AuthorNotePresetPopup.svelte'

const mocks = vi.hoisted(() => ({
    confirm: vi.fn(async () => true),
    actionConfirm: vi.fn(async () => true),
    input: vi.fn(async () => ''),
    toast: vi.fn(),
    error: vi.fn(),
    save: vi.fn(async () => {}),
    flush: vi.fn(async () => {}),
    usage: vi.fn(async (): Promise<AuthorNotePresetUsage[]> => []),
    close: vi.fn(),
}))
vi.mock('src/ts/stores.svelte', async () => {
    const { writable } = await import('svelte/store')
    const DBState = $state({ db: {} })
    return { DBState, alertStore: writable({ type: 'none', msg: '' }) }
})
vi.mock('src/ts/alert', () => ({
    alertConfirm: mocks.confirm,
    alertActionConfirm: mocks.actionConfirm,
    alertInput: mocks.input,
    alertToast: mocks.toast,
    alertError: mocks.error,
}))
vi.mock('src/ts/chatBindings.svelte', () => ({
    captureChatBindingTarget: () => ({ conversation: DBState.db.characters[0].chats[0], isCurrent: () => true }),
    updateChatBinding: async (conversation: object, patch: object) => applyConversationBindingPatch(conversation, patch),
    saveChatBinding: mocks.save,
    chatBindingBlockedByGeneration: () => false,
}))
vi.mock('src/ts/storage/persistentDataRuntime.svelte', () => ({
    flushPendingData: mocks.flush,
    getPersistentDataRuntime: () => ({ store: { queryAuthorNotePresetUsage: mocks.usage } }),
}))

let instance: ReturnType<typeof mount>
const chat = () => DBState.db.characters[0].chats[0]
const other = () => DBState.db.characters[1].chats[0]
const note = (conversation = chat()) => effectiveAuthorNote(DBState.db.authorNotePresets, conversation)
const rows = () => [...document.querySelectorAll<HTMLButtonElement>(`button[title="${languageEnglish.apply}"]`)]
const byText = (text: string) =>
    [...document.querySelectorAll('button')].find((button) => button.textContent?.trim() === text)!
const openActions = async (row: number) => {
    document.querySelectorAll<HTMLButtonElement>(`button[title="${languageEnglish.presetActions}"]`)[row].click()
    await tick()
}
const link = (id: string, text: string) => ({ note: text, authorNotePreset: { id, fingerprint: authorNoteFingerprint(text) } })

beforeEach(async () => {
    DBState.db = {
        authorNotePresets: [
            { id: 'calm', name: 'Calm', text: 'Speak calmly.' },
            { id: 'brief', name: 'Brief', text: 'Answer briefly.' },
        ],
        characters: [
            { chaId: 'first', chatPage: 0, chats: [{ id: 'chat', note: 'My own note' }] },
            { chaId: 'second', chatPage: 0, chats: [{ id: 'other', ...link('calm', 'Speak calmly.') }] },
        ],
    } as unknown as typeof DBState.db
    instance = mount(AuthorNotePresetPopup, { target: document.body, props: { close: mocks.close } })
    await tick()
})

afterEach(async () => {
    await unmount(instance)
    document.body.replaceChildren()
    vi.clearAllMocks()
})

it('asks before a preset replaces the chat note and then reads the preset like other chats', async () => {
    mocks.confirm.mockResolvedValueOnce(false)
    rows()[0].click()
    await vi.waitFor(() => expect(mocks.confirm).toHaveBeenCalledWith(languageEnglish.selectAuthorNotePresetConfirm))
    expect(note()).toBe('My own note')

    rows()[0].click()
    await vi.waitFor(() => expect(mocks.close).toHaveBeenCalled())
    expect(note()).toBe('Speak calmly.')
    DBState.db.authorNotePresets![0].text = 'Edited for both'
    expect([note(), note(other())]).toEqual(['Edited for both', 'Edited for both'])
    expect(mocks.save).toHaveBeenCalledTimes(1)
})

it('switches presets and selects an empty chat without asking', async () => {
    chat().note = ''
    rows()[1].click()
    await vi.waitFor(() => expect(note()).toBe('Answer briefly.'))
    rows()[0].click()
    await vi.waitFor(() => expect(note()).toBe('Speak calmly.'))
    expect(mocks.confirm).not.toHaveBeenCalled()
})

it('stores the current preset text in the chat when unlinked', async () => {
    Object.assign(chat(), link('calm', 'old copy'))
    await tick()
    expect(rows()[0].getAttribute('aria-current')).toBe('true')
    byText(languageEnglish.authorNoteStoredInChat).click()
    await vi.waitFor(() => expect(chat().authorNotePreset).toBeUndefined())
    DBState.db.authorNotePresets![0].text = 'Later edit'
    expect(note()).toBe('Speak calmly.')
})

it('saves the current note as a new preset selected by this chat', async () => {
    mocks.input.mockResolvedValueOnce(' Mine ')
    byText(languageEnglish.saveNewPreset).click()
    await vi.waitFor(() => expect(mocks.close).toHaveBeenCalled())
    const created = DBState.db.authorNotePresets![2]
    expect(created).toEqual({ id: expect.any(String), name: 'Mine', text: 'My own note' })
    expect(chat().authorNotePreset?.id).toBe(created.id)
    expect(mocks.confirm).not.toHaveBeenCalled()
})

it('duplicates a preset as a separate preset', async () => {
    await openActions(0)
    byText(languageEnglish.duplicatePreset).click()
    await tick()
    const [original, copy] = DBState.db.authorNotePresets!
    expect(copy).toEqual({ id: expect.any(String), name: 'Calm (Copy)', text: 'Speak calmly.' })
    expect(copy.id).not.toBe(original.id)
    copy.text = 'Only the copy'
    expect(note(other())).toBe('Speak calmly.')
})

it('lists the chats that use a preset before deleting it and empties their note', async () => {
    const usage = Array.from({ length: 11 }, (_, index) => ({
        characterId: 'second', characterName: 'Second', conversationId: `chat-${index}`, conversationName: `Chat ${index}`,
    }))
    mocks.usage.mockResolvedValueOnce(usage)
    Object.assign(chat(), link('calm', 'Speak calmly.'))
    await openActions(0)
    byText(languageEnglish.deletePreset).click()
    await vi.waitFor(() => expect(DBState.db.authorNotePresets).toHaveLength(1))
    expect(mocks.flush.mock.invocationCallOrder[0]).toBeLessThan(mocks.usage.mock.invocationCallOrder[0])
    expect(mocks.usage).toHaveBeenCalledWith('calm')
    const { description, title } = (mocks.actionConfirm.mock.calls[0] as unknown as [{ description: string; title: string }])[0]
    expect(title).toBe(languageEnglish.deleteAuthorNotePresetConfirm('Calm'))
    expect(description.split('\n')).toEqual([
        languageEnglish.deleteUsedAuthorNotePresetDescription,
        '',
        ...usage.slice(0, 10).map((item) => `Second · ${item.conversationName}`),
        languageEnglish.authorNotePresetUsageMore(1),
    ])
    expect(note(other())).toBe('')
    expect(note()).toBe('')
    await tick()
    expect(byText(languageEnglish.authorNoteStoredInChat).getAttribute('aria-current')).toBe('true')
})

it('asks the plain question for a preset no chat uses', async () => {
    await openActions(1)
    byText(languageEnglish.deletePreset).click()
    await vi.waitFor(() => expect(DBState.db.authorNotePresets).toHaveLength(1))
    expect(mocks.confirm).toHaveBeenCalledWith(languageEnglish.deleteAuthorNotePresetConfirm('Brief'))
    expect(mocks.actionConfirm).not.toHaveBeenCalled()
})
