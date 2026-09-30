import { beforeEach, describe, expect, it, vi } from 'vitest'
import { DBState } from './stores.svelte'
import { selectSingleFile } from './util'
import { saveImage } from './storage/database.svelte'
import { changeUserPersona, selectUserImg } from './persona'

vi.mock('./stores.svelte', () => ({ DBState: { db: {} } }))
vi.mock('./util', () => ({ selectSingleFile: vi.fn() }))
vi.mock('./storage/database.svelte', () => ({ saveImage: vi.fn() }))
vi.mock('./alert', () => ({}))
vi.mock('./globalApi.svelte', () => ({}))
vi.mock('src/lang', () => ({ language: {} }))
vi.mock('./process/files/inlays', () => ({}))
vi.mock('./pngChunk', () => ({}))

beforeEach(() => {
    vi.mocked(saveImage).mockResolvedValue('assets/new-icon.png')
    vi.mocked(selectSingleFile).mockResolvedValue({ name: 'synthetic.png', data: new Uint8Array([1]) })
    DBState.db = {
        personas: [
            { id: 'persona-a', name: 'A', icon: 'a.png', personaPrompt: 'A prompt', note: '' },
            { id: 'persona-b', name: 'B', icon: 'b.png', personaPrompt: 'B prompt', note: '' },
        ], selectedPersona: 0, username: 'A', userIcon: 'a.png', personaPrompt: 'A prompt', userNote: '',
        characters: [{ chatPage: 0, chats: [{ bindedPersona: 'persona-a' }] }],
    } as any
})

describe('persona image edits', () => {
    it('keeps the persona ID used by existing chat bindings', async () => {
        await selectUserImg()
        expect(DBState.db.personas[0].id).toBe('persona-a')
        expect(DBState.db.personas[0].icon).toBe('assets/new-icon.png')
        expect(DBState.db.userIcon).toBe('assets/new-icon.png')
        expect(DBState.db.characters[0].chats[0].bindedPersona).toBe(DBState.db.personas[0].id)
    })

    it('applies a delayed image to its original persona without changing the newly selected one', async () => {
        let resolve!: (value: string) => void
        vi.mocked(saveImage).mockImplementationOnce(() => new Promise(done => { resolve = done }))
        const editing = selectUserImg()
        await vi.waitFor(() => expect(resolve).toBeDefined())
        changeUserPersona(1)
        resolve('assets/new-icon.png')
        await editing
        expect(DBState.db.personas[0].icon).toBe('assets/new-icon.png')
        expect(DBState.db.personas[1].icon).toBe('b.png')
        expect(DBState.db.userIcon).toBe('b.png')
        expect(DBState.db.personas.map(persona => persona.id)).toEqual(['persona-a', 'persona-b'])
    })
})
