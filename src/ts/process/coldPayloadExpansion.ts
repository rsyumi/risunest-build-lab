import { normalizePocketColdPayload } from '../drive/pocketRisuFeatures'
import type { Database, character } from '../storage/database.svelte'
import { defineOwnEnumerableProperty } from '../storage/ownEnumerableProperty'
import { coldStorageHeader, isColdStorageBackupData } from './coldstorageData'

type ColdCharacter = Database['characters'][number]

/** A cold item read: `present` carries any JSON value, `null` included. */
export type ColdItemRead =
    | { kind: 'present', value: unknown }
    | { kind: 'missing' }
    | { kind: 'unreadable' }
    | { kind: 'too-large' }

export type ColdItemReader = (key: string) => Promise<ColdItemRead>

export function coldItemFromMap(items: ReadonlyMap<string, unknown>, key: string): ColdItemRead {
    return items.has(key) ? { kind: 'present', value: items.get(key) } : { kind: 'missing' }
}

export const PLUGIN_STORAGE_INDEX_KEY = '_coldpluginIndex'
export const PLUGIN_STORAGE_LEGACY_MAP_KEY = '_coldplugin'
const PLUGIN_STORAGE_INDEX_TYPE = 'risu-plugin-storage-index'

export interface ColdPayloadExpansion {
    expandedKeys: string[]
    unavailableKeys: string[]
}

function chatColdKey(chat: unknown): string | null {
    const data = (chat as { message?: { data?: string }[] })?.message?.[0]?.data
    if (typeof data !== 'string' || !data.startsWith(coldStorageHeader)) return null
    return data.slice(coldStorageHeader.length)
}

function applyChatPayload(chat: Record<string, unknown>, payload: unknown): boolean {
    if (Array.isArray(payload)) {
        chat.message = payload
        return true
    }
    if (!payload || typeof payload !== 'object') return false
    const source = payload as Record<string, unknown>
    if (!Array.isArray(source.message)) return false
    chat.message = source.message
    if (Object.hasOwn(source, 'savedToggleValues')) chat.savedToggleValues = source.savedToggleValues
    if (Object.hasOwn(source, 'bindedPersona')) chat.bindedPersona = source.bindedPersona
    chat.hypaV2Data = source.hypaV2Data
    chat.hypaV3Data = source.hypaV3Data
    chat.scriptstate = source.scriptstate
    chat.localLore = source.localLore
    return true
}

function payloadCharacter(payload: unknown): character | null {
    if (!payload || typeof payload !== 'object' || Array.isArray(payload)) return null
    const value = (payload as { character?: unknown }).character
    if (!value || typeof value !== 'object' || Array.isArray(value)) return null
    return value as character
}

function isAbortError(error: unknown): boolean {
    return (error as { name?: string })?.name === 'AbortError'
}

/**
 * Expands upstream cold storage references into the records that carry them.
 * Unresolved references remain available for retry or an explicit incomplete restore.
 */
export async function expandColdPayloads(
    db: Pick<Database, 'characters'>,
    read: ColdItemReader,
): Promise<ColdPayloadExpansion> {
    const expandedKeys: string[] = []
    const unavailableKeys: string[] = []
    const characters = db?.characters
    if (!Array.isArray(characters)) return { expandedKeys, unavailableKeys }

    const load = async (key: string): Promise<unknown | null> => {
        let item: ColdItemRead
        try {
            item = await read(key)
        } catch (error) {
            if (isAbortError(error)) throw error
            console.error(`Failed to read the cold storage payload ${key}:`, error)
            unavailableKeys.push(key)
            return null
        }
        const value = item.kind === 'present' ? item.value : null
        if (value === null || value === undefined || !isColdStorageBackupData(value)) {
            unavailableKeys.push(key)
            return null
        }
        try {
            return normalizePocketColdPayload(value)
        } catch (error) {
            console.error(`Cold storage payload ${key} has an unsupported shape:`, error)
            unavailableKeys.push(key)
            return null
        }
    }

    for (let index = 0; index < characters.length; index++) {
        let current = characters[index]
        if (!current) continue

        const characterKey = current.coldstorage
        if (characterKey) {
            const payload = await load(characterKey)
            const restored = payloadCharacter(payload)
            if (restored) {
                characters[index] = restored as ColdCharacter
                current = characters[index]
                expandedKeys.push(characterKey)
                delete current.coldstorage
            } else if (payload !== null) {
                unavailableKeys.push(characterKey)
            }
        }

        for (const chat of current.chats ?? []) {
            const chatKey = chatColdKey(chat)
            if (!chatKey) continue
            const payload = await load(chatKey)
            if (payload === null) continue
            if (applyChatPayload(chat as unknown as Record<string, unknown>, payload)) {
                expandedKeys.push(chatKey)
            } else {
                unavailableKeys.push(chatKey)
            }
        }

        if (!current.coldstorage && !(current.chats ?? []).some(chat => chatColdKey(chat))) {
            delete current.coldStoragedChats
        }
    }

    return { expandedKeys, unavailableKeys }
}

type PluginStorageIndexEntry = { s?: unknown, l?: unknown }

function isPluginStorageIndex(value: unknown): value is { entries: unknown[], trash: unknown[] } {
    return !!value
        && typeof value === 'object'
        && (value as { type?: unknown }).type === PLUGIN_STORAGE_INDEX_TYPE
        && Array.isArray((value as { entries?: unknown }).entries)
        && Array.isArray((value as { trash?: unknown }).trash)
}

/**
 * Resolves upstream cold plugin storage (the index and the legacy per-key map)
 * into plain plugin values and removes both reference keys. Precedence is inline,
 * then index, then legacy. A value that cannot be resolved is counted, never stored;
 * one that is too large is counted separately.
 */
export async function importColdPluginStorage(
    db: { pluginCustomStorage?: Database['pluginCustomStorage'] },
    read: ColdItemReader,
): Promise<{ omitted: number, tooLarge: number }> {
    const storage = db?.pluginCustomStorage
    if (!storage || typeof storage !== 'object' || Array.isArray(storage)) return { omitted: 0, tooLarge: 0 }
    const hasIndex = Object.hasOwn(storage, PLUGIN_STORAGE_INDEX_KEY)
    const hasLegacy = Object.hasOwn(storage, PLUGIN_STORAGE_LEGACY_MAP_KEY)
    if (!hasIndex && !hasLegacy) return { omitted: 0, tooLarge: 0 }
    const pointer: unknown = storage[PLUGIN_STORAGE_INDEX_KEY]
    const legacy: unknown = storage[PLUGIN_STORAGE_LEGACY_MAP_KEY]
    delete storage[PLUGIN_STORAGE_INDEX_KEY]
    delete storage[PLUGIN_STORAGE_LEGACY_MAP_KEY]

    let omitted = 0
    let tooLarge = 0
    const skip = (item: ColdItemRead) => {
        if (item.kind === 'too-large') tooLarge++
        else omitted++
    }
    const readItem = async (key: string): Promise<ColdItemRead> => {
        try {
            return await read(key)
        } catch (error) {
            if (isAbortError(error)) throw error
            console.error(`Failed to read the plugin storage item ${key}:`, error)
            return { kind: 'unreadable' }
        }
    }
    const isReserved = (key: string) => key === PLUGIN_STORAGE_INDEX_KEY || key === PLUGIN_STORAGE_LEGACY_MAP_KEY
    const hasInline = (key: string) => Object.hasOwn(storage, key) && storage[key] !== undefined
    const inlineKeys = new Set(Object.keys(storage).filter(hasInline))

    const indexed = new Map<string, PluginStorageIndexEntry>()
    if (typeof pointer === 'string') {
        const index = await readItem(pointer)
        if (index.kind === 'present' && isPluginStorageIndex(index.value)) {
            for (const entry of index.value.entries as { k?: unknown, s?: unknown, l?: unknown }[]) {
                if (typeof entry?.k !== 'string' || isReserved(entry.k)
                    || (typeof entry.l !== 'string' && typeof entry.s !== 'string')) {
                    omitted++
                    continue
                }
                indexed.set(entry.k, entry)
            }
        } else {
            skip(index)
        }
    } else if (pointer !== null && pointer !== undefined) {
        omitted++
    }

    for (const [key, entry] of indexed) {
        if (inlineKeys.has(key)) continue
        if (typeof entry.l === 'string') {
            const item = await readItem(entry.l)
            if (item.kind !== 'present') {
                skip(item)
                continue
            }
            defineOwnEnumerableProperty(storage, key, item.value)
            continue
        }
        try {
            defineOwnEnumerableProperty(storage, key, JSON.parse(entry.s as string))
        } catch {
            omitted++
        }
    }

    if (legacy !== null && legacy !== undefined) {
        if (typeof legacy !== 'object' || Array.isArray(legacy)) {
            omitted++
        } else {
            for (const [key, id] of Object.entries(legacy)) {
                if (inlineKeys.has(key) || indexed.has(key)) continue
                if (typeof id !== 'string' || isReserved(key)) {
                    omitted++
                    continue
                }
                const item = await readItem(id)
                if (item.kind !== 'present') {
                    skip(item)
                    continue
                }
                defineOwnEnumerableProperty(storage, key, item.value)
            }
        }
    }

    return { omitted, tooLarge }
}
