import { afterEach, expect, it, vi } from 'vitest'
import { mount, tick, unmount } from 'svelte'
import { writable } from 'svelte/store'
import { IDBFactory, IDBKeyRange } from 'fake-indexeddb'
import { languageEnglish } from 'src/lang/en'
import { DBState } from 'src/ts/stores.svelte'
import PersonaSettings from './PersonaSettings.svelte'
import { IndexedDbPersistentDataStore } from 'src/ts/storage/indexedDbPersistentDataStore'
import { SaveCoordinator } from 'src/ts/storage/saveCoordinator'
import { capturePersistentRoot } from 'src/ts/storage/persistentDataRuntime'
import { alertConfirm } from 'src/ts/alert'

vi.mock('src/lang', () => ({ language: languageEnglish }))
vi.mock('src/ts/stores.svelte', () => {
    const DBState = $state({ db: {} })
    return { DBState, disableHighlight: writable(false), popUpEditorStore: {} }
})
vi.mock('sortablejs/modular/sortable.core.esm.js', () => ({ default: { create: () => ({ destroy() {} }) } }))
vi.mock('src/ts/util', () => ({ sortableOptions: {}, sleep: async () => {} }))
vi.mock('src/ts/alert', () => ({ alertConfirm: vi.fn() }))
vi.mock('src/ts/characters', () => ({ getCharImage: vi.fn(async () => '') }))
vi.mock('src/ts/storage/database.svelte', () => ({}))
vi.mock('src/ts/globalApi.svelte', () => ({}))
vi.mock('src/ts/process/files/inlays', () => ({}))
vi.mock('src/ts/pngChunk', () => ({}))
vi.mock('src/ts/gui/highlight', () => ({ highlighter() {}, removeHighlight() {}, getNewHighlightId: () => 1, AllCBS: [] }))
vi.mock('src/ts/gui/guisize', () => ({ textAreaSize: writable(0), textAreaTextSize: writable(0) }))
vi.mock('src/ts/hotkey', () => ({ hotkeyMatches: () => false }))

let instance: ReturnType<typeof mount>
afterEach(async () => {
    if (instance) await unmount(instance)
    document.body.replaceChildren()
})

it('removes the confirmed persona after the active selection changes', async () => {
    DBState.db = {
        personas: ['First', 'Second', 'Third'].map(id => ({ id, name: id, icon: '', personaPrompt: '', note: '' })),
        selectedPersona: 0, username: 'First', userIcon: '', personaPrompt: '', userNote: '', hotkeys: [],
    } as any
    let confirm!: (value: boolean) => void
    vi.mocked(alertConfirm).mockReturnValueOnce(new Promise(resolve => { confirm = resolve }))
    instance = mount(PersonaSettings, { target: document.body })
    await tick()
    const remove = [...document.querySelectorAll('button')].find(button => button.textContent === languageEnglish.remove)!
    remove.click()
    DBState.db.selectedPersona = 1
    DBState.db.username = 'Second'
    await tick()
    confirm(true)
    await vi.waitFor(() => expect(DBState.db.personas).toHaveLength(2))
    expect(DBState.db.personas.map(persona => persona.id)).toEqual(['Second', 'Third'])
    expect(DBState.db.personas[DBState.db.selectedPersona].id).toBe('Second')
})

it('updates the persona used by bound chats while editing its current name, note and prompt', async () => {
    DBState.db = {
        personas: [{ id: 'persona', name: 'Before', icon: '', personaPrompt: 'Before prompt', note: 'Before note' }],
        selectedPersona: 0, username: 'Before', userIcon: '', personaPrompt: 'Before prompt', userNote: 'Before note',
        personaNote: true, hotkeys: [], characters: [], botPresets: [],
    } as any
    const indexedDB = new IDBFactory()
    const store = new IndexedDbPersistentDataStore('persona-editor', indexedDB, IDBKeyRange)
    await store.open()
    const { revision } = await store.replaceFromDatabase($state.snapshot(DBState.db))
    const coordinator = new SaveCoordinator({
        store, captureRoot: () => capturePersistentRoot(DBState.db),
        captureCharacter: () => null,
        captureSelectedCharacter: () => null, replaceDatabase: () => {},
    })
    coordinator.initialize(revision)
    instance = mount(PersonaSettings, { target: document.body })
    await tick()
    const inputs = document.querySelectorAll<HTMLInputElement>('input[type="text"]')
    const prompt = document.querySelector('textarea')!
    inputs[0].focus()
    inputs[0].value = 'Edited name'
    inputs[0].dispatchEvent(new Event('input', { bubbles: true }))
    inputs[1].value = 'Edited note'
    inputs[1].dispatchEvent(new Event('input', { bubbles: true }))
    prompt.value = 'Edited prompt'
    prompt.dispatchEvent(new Event('input', { bubbles: true }))
    await tick()
    expect(DBState.db.personas[0]).toMatchObject({ id: 'persona', name: 'Edited name', note: 'Edited note', personaPrompt: 'Edited prompt' })
    expect(document.activeElement).toBe(inputs[0])
    expect(document.querySelector('textarea')).toBe(prompt)
    coordinator.markPersistentDataDirty(0)
    await coordinator.flushPendingDataLocally('persona-editor-test')
    const reopened = new IndexedDbPersistentDataStore('persona-editor', indexedDB, IDBKeyRange)
    await reopened.open()
    expect((await reopened.readRoot()).value.personas).toEqual($state.snapshot(DBState.db.personas))
    await unmount(instance)
    instance = mount(PersonaSettings, { target: document.body })
    await tick()
    expect(document.querySelector<HTMLInputElement>('input[type="text"]')!.value).toBe('Edited name')
    expect(document.querySelector('textarea')!.value).toBe('Edited prompt')
})
