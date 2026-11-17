import { expect, it } from 'vitest'
import {
    authorNoteFingerprint,
    authorNotePresetLinkPatch,
    effectiveAuthorNote,
    materializeAuthorNote,
    writeAuthorNote,
} from './authorNotePresets'
import type { Chat } from './storage/database.svelte'

type NoteChat = Pick<Chat, 'note' | 'authorNotePreset'>
const preset = { id: 'preset', name: 'Shared', text: 'Shared note' }
const linked = (note = 'stale copy'): NoteChat => ({ note, authorNotePreset: { id: 'preset', fingerprint: authorNoteFingerprint(note) } })

it('fingerprints UTF-16 code units the same way as the compatible export', () => {
    expect(['', 'a', 'Hello, 세계', '😀', 'a\r\nb'].map(authorNoteFingerprint)).toEqual([
        '0:811c9dc5', '1:e40c292c', '9:97908cb3', '2:cb31c4b8', '4:c40663db',
    ])
    expect(authorNoteFingerprint(undefined)).toBe(authorNoteFingerprint(''))
})

it('reads the preset while the stored copy is untouched and the chat note once it is edited elsewhere', () => {
    expect(effectiveAuthorNote([preset], linked())).toBe('Shared note')
    expect(effectiveAuthorNote([preset], { ...linked(), note: 'edited by a script' })).toBe('edited by a script')
    expect(effectiveAuthorNote([], linked())).toBe('')
    expect(effectiveAuthorNote([{ id: 'preset', text: 'no name' }], linked())).toBe('')
    expect(effectiveAuthorNote([preset], { note: 'own note' })).toBe('own note')
    expect(effectiveAuthorNote([preset], { note: undefined as unknown as string, authorNotePreset: { id: 'preset', fingerprint: authorNoteFingerprint('') } })).toBe('Shared note')
})

it('writes a linked edit through to the preset and an unlinked edit to the chat alone', () => {
    const presets = [{ ...preset }]
    const first = { ...linked(), ...authorNotePresetLinkPatch(presets[0]) }
    const second = linked('older copy')
    writeAuthorNote(presets, first, 'Edited together')
    expect(presets[0].text).toBe('Edited together')
    expect(effectiveAuthorNote(presets, first)).toBe('Edited together')
    expect(effectiveAuthorNote(presets, second)).toBe('Edited together')

    const edited = { ...linked(), note: 'changed elsewhere' }
    writeAuthorNote(presets, edited, 'stale copy')
    expect(edited).toEqual({ note: 'stale copy' })
    expect(presets[0].text).toBe('Edited together')
})

it('exports the note a chat reads without the link', () => {
    expect(materializeAuthorNote([preset], { id: 'chat', ...linked() })).toEqual({ id: 'chat', note: 'Shared note' })
    expect(materializeAuthorNote([], { id: 'chat', ...linked() })).toEqual({ id: 'chat', note: '' })
})
