import { v4 } from 'uuid'
import type { Chat, Database } from './storage/database.svelte'

export type AuthorNotePreset = NonNullable<Database['authorNotePresets']>[number]
export type AuthorNotePresetLink = NonNullable<Chat['authorNotePreset']>
type AuthorNoteChat = Pick<Chat, 'note' | 'authorNotePreset'>

export type AuthorNoteSource =
    | { kind: 'chat' }
    | { kind: 'preset'; preset: AuthorNotePreset }
    | { kind: 'deleted' }

/** Length and FNV-1a over UTF-16 code units; the Rust compatible export computes the same value. */
export function authorNoteFingerprint(text: string | undefined): string {
    const value = typeof text === 'string' ? text : ''
    let hash = 0x811c9dc5
    for (let index = 0; index < value.length; index++) {
        hash ^= value.charCodeAt(index)
        hash = Math.imul(hash, 0x01000193)
    }
    return `${value.length}:${(hash >>> 0).toString(16).padStart(8, '0')}`
}

export function isAuthorNotePreset(value: unknown): value is AuthorNotePreset {
    const preset = value as AuthorNotePreset | null
    return !!preset && typeof preset === 'object' && typeof preset.id === 'string'
        && typeof preset.name === 'string' && typeof preset.text === 'string'
}

/**
 * A note that no longer matches the fingerprint written with the link was changed outside the
 * preset UI, so the chat reads its own note again.
 */
export function resolveAuthorNoteSource(
    presets: readonly unknown[] | undefined,
    chat: AuthorNoteChat,
): AuthorNoteSource {
    const id = matchingAuthorNotePresetId(chat)
    if (id === null) return { kind: 'chat' }
    const preset = presets?.find((item) => isAuthorNotePreset(item) && item.id === id)
    return preset ? { kind: 'preset', preset: preset as AuthorNotePreset } : { kind: 'deleted' }
}

/** The preset id a chat reads from, whether or not that preset still exists. */
export function matchingAuthorNotePresetId(chat: AuthorNoteChat): string | null {
    const link = chat.authorNotePreset
    return link && typeof link.id === 'string' && link.fingerprint === authorNoteFingerprint(chat.note)
        ? link.id
        : null
}

export function effectiveAuthorNote(presets: readonly unknown[] | undefined, chat: AuthorNoteChat): string {
    const source = resolveAuthorNoteSource(presets, chat)
    if (source.kind === 'preset') return source.preset.text
    return source.kind === 'chat' ? chat.note ?? '' : ''
}

export function linkedAuthorNotePreset(
    presets: readonly unknown[] | undefined,
    chat: AuthorNoteChat,
): AuthorNotePreset | null {
    const source = resolveAuthorNoteSource(presets, chat)
    return source.kind === 'preset' ? source.preset : null
}

/** The conversation fields that select a preset; `note` keeps a copy for readers that do not resolve it. */
export function authorNotePresetLinkPatch(preset: Pick<AuthorNotePreset, 'id' | 'text'>, text = preset.text) {
    return { note: text, authorNotePreset: { id: preset.id, fingerprint: authorNoteFingerprint(text) } }
}

/** Stores an edited note; a linked chat writes through to its preset, so every chat that selected it reads the edit. */
export function writeAuthorNote(presets: readonly unknown[] | undefined, chat: AuthorNoteChat, value: string): void {
    const linked = linkedAuthorNotePreset(presets, chat)
    if (linked) {
        linked.text = value
        chat.authorNotePreset = { id: linked.id, fingerprint: authorNoteFingerprint(value) }
    } else if (chat.authorNotePreset) {
        delete chat.authorNotePreset
    }
    chat.note = value
}

export function createAuthorNotePreset(name: string, text: string): AuthorNotePreset {
    return { id: v4(), name, text }
}

/** A copy of the chat that stores its effective note itself, for files read outside this library. */
export function materializeAuthorNote<T extends AuthorNoteChat>(presets: readonly unknown[] | undefined, chat: T): T {
    const copy = { ...chat, note: effectiveAuthorNote(presets, chat) }
    delete copy.authorNotePreset
    return copy
}
