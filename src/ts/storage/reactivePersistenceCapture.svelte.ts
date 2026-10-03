import { isConversationSummaryStub } from './conversationResidency'
import { isTauri } from '../platform'
import { untrack } from 'svelte'
import type { Chat, character, groupChat } from './database.svelte'
import type { RootMutation } from './persistentDataStore'
import {
    canonicalJson,
    pluginStorageJson,
    type PluginStorageCapture,
} from './saveCoordinatorHelpers'
import { defineOwnEnumerableProperty } from './ownEnumerableProperty'
import { diffRootMutations } from './rootMutation'

/** Production-only: read closures must expose the deeply reactive DBState working set. */
export interface PersistenceCanonicalCapture {
    materializedCharacters?(): ReadonlyMap<string, character | groupChat>
    characters?(): ReadonlyMap<string, string>
    root(): string
    rootFields?(canonical: string): ReadonlyMap<string, unknown> | undefined
    diffRoot(before: string, after: string): RootMutation[]
    presets(): string | null
    character(): string | null
    pluginStorage(): PluginStorageCapture | null
    seedPluginStorage?(capture: PluginStorageCapture): void
}

// Applying $state to an existing deep state proxy preserves its identity.
// Raw/plain objects produce a new proxy and must be serialized afresh.
function isDeepState(value: object): boolean {
    const candidate = $state(value)
    return candidate === value
}

const volatileValue = Symbol('volatile persistence value')

function normalize(value: unknown, markVolatile: () => never): unknown {
    if (typeof value === 'function') markVolatile()
    if (!value || typeof value !== 'object') return value
    const prototype = Object.getPrototypeOf(value)
    if (prototype !== Object.prototype && prototype !== Array.prototype) markVolatile()
    if (!isDeepState(value)) markVolatile()
    if (Array.isArray(value)) {
        return value.map((entry, index) => {
            if (Object.getOwnPropertyDescriptor(value, String(index))?.get) markVolatile()
            return normalize(entry, markVolatile)
        })
    }
    const output: Record<string, unknown> = {}
    for (const key of Object.keys(value).sort()) {
        if (Object.getOwnPropertyDescriptor(value, key)?.get) markVolatile()
        const entry = (value as Record<string, unknown>)[key]
        if (entry !== undefined)
            defineOwnEnumerableProperty(output, key, normalize(entry, markVolatile))
    }
    return output
}

function fieldCapture(
    read: () => unknown,
    takeStringSeed?: () => readonly [string, string] | undefined,
) {
    const capture = () => {
        try {
            const value = normalize(read(), () => {
                throw volatileValue
            })
            const seed = takeStringSeed?.()
            return {
                value,
                json:
                    typeof value === 'string' && seed?.[0] === value
                        ? seed[1]
                        : (JSON.stringify(value) as string | undefined),
                volatile: false,
            }
        } catch (error) {
            if (error !== volatileValue) throw error
            return { value: undefined, json: undefined, volatile: true }
        }
    }
    const cached = $derived.by(capture)
    return () => cached
}

function objectCapture(
    read: () => Record<string, unknown> | null,
    omit: ReadonlySet<string>,
    ordered: boolean,
    stringSeeds?: Map<string, readonly [string, string]>,
    transform?: (key: string, value: unknown) => unknown,
) {
    const fields = new Map<string, ReturnType<typeof fieldCapture>>()
    let previous:
        | { volatile: false; json: string; entries: readonly (readonly [string, string])[]; values: ReadonlyMap<string, unknown> }
        | undefined
    const snapshot = () => {
        const source = read()
        if (source === null) {
            stringSeeds?.clear()
            fields.clear()
            previous = undefined
            return null
        }
        if (!isDeepState(source)) {
            stringSeeds?.clear()
            fields.clear()
            previous = undefined
            return { volatile: true as const }
        }
        const keys = Object.keys(source).filter((key) => !omit.has(key))
        // Object JSON visits integer keys first, even after canonical sorting.
        const order: Record<string, boolean> = {}
        for (const key of ordered ? keys : keys.sort())
            defineOwnEnumerableProperty(order, key, true)
        const currentKeys = Object.keys(order)
        const active = new Set(currentKeys)
        for (const key of fields.keys()) if (!active.has(key)) fields.delete(key)
        const entries: Array<readonly [string, string]> = []
        const values = new Map<string, unknown>()
        let volatile = false
        for (const key of currentKeys) {
            let field = fields.get(key)
            if (!field) {
                field = untrack(() =>
                    fieldCapture(
                        () => {
                            const value = read()
                            // Do not evaluate an external getter inside a cached derived.
                            if (value && Object.getOwnPropertyDescriptor(value, key)?.get)
                                throw volatileValue
                            return transform ? transform(key, value?.[key]) : value?.[key]
                        },
                        () => {
                            const seed = stringSeeds?.get(key)
                            stringSeeds?.delete(key)
                            return seed
                        },
                    ),
                )
                fields.set(key, field)
            }
            const captured = field()
            volatile ||= captured.volatile
            if (captured.json !== undefined) {
                entries.push(Object.freeze([key, captured.json]))
                values.set(key, captured.value)
            }
        }
        stringSeeds?.clear()
        if (volatile) return { volatile: true as const }
        if (
            previous &&
            entries.length === previous.entries.length &&
            entries.every(
                ([key, json], index) =>
                    key === previous!.entries[index][0] && json === previous!.entries[index][1],
            )
        )
            return previous
        let json: string | undefined
        previous = {
            volatile: false as const,
            get json() {
                return (json ??=
                    '{' +
                    entries.map(([key, json]) => JSON.stringify(key) + ':' + json).join(',') +
                    '}')
            },
            entries: Object.freeze(entries),
            values,
        }
        return previous
    }
    return () => {
        const captured = snapshot()
        if (!captured) return null
        if (captured.volatile === false) return captured
        // Plain objects, getters and callable hooks can change without a reactive
        // notification. Preserve their full-object JSON semantics on every read.
        const source = read()
        if (source === null) return null
        const value: Record<string, unknown> = {}
        for (const key of Object.keys(source))
            if (!omit.has(key)) defineOwnEnumerableProperty(value, key, transform ? transform(key, source[key]) : source[key])
        return { json: ordered ? pluginStorageJson(value) : canonicalJson(value), entries: null, values: undefined }
    }
}

export function createPersistenceCanonicalCapture(read: {
    root(): object
    pluginStorage(): Record<string, unknown> | null
    presets(): unknown
    character(): unknown
    characters?(): readonly (character | groupChat)[]
    rootField?(key: string, value: unknown): unknown
}): PersistenceCanonicalCapture {
    const captureRoot = objectCapture(
        () => read.root() as Record<string, unknown>,
        new Set(['characters', 'botPresets', 'pluginCustomStorage', 'pluginStorageMeta', ...(isTauri ? ['account'] : [])]),
        false,
        undefined,
        read.rootField,
    )
    const characterCaptures = new Map<string, { value: character | groupChat; capture: () => {json:string; value:character | groupChat} }>()
    const captureCharacters = () => {
        const result = new Map<string, {json:string; value:character | groupChat}>()
        for (const value of read.characters?.() ?? []) {
            let entry = characterCaptures.get(value.chaId)
            if (!entry || entry.value !== value) {
                const detail = untrack(() => objectCapture(() => value as unknown as Record<string, unknown>, new Set(['chats']), false))
                const chats = new Map<Chat, ReturnType<typeof objectCapture>>()
                const decoded = new Map<string, {json:string; value:unknown}>()
                const decode = (key: string, json: string) => {
                    let previous = decoded.get(key)
                    if (previous?.json !== json) { previous = {json, value: JSON.parse(json)}; decoded.set(key, previous) }
                    return previous!.value
                }
                let previous: {json:string; value:character | groupChat} | undefined
                let previousParts: readonly unknown[] = []
                const capture = () => {
                    const capturedDetail = detail()!
                    const active = new Set(value.chats)
                    for (const chat of chats.keys()) if (!active.has(chat)) chats.delete(chat)
                    const capturedChats = value.chats.map((chat) => {
                        let capturedChat = chats.get(chat)
                        if (!capturedChat) {
                            const omit = isConversationSummaryStub(chat) || !Object.prototype.propertyIsEnumerable.call(chat, 'message') ? new Set(['message']) : new Set<string>()
                            capturedChat = untrack(() => objectCapture(() => chat as unknown as Record<string, unknown>, omit, false))
                            chats.set(chat, capturedChat)
                        }
                        return capturedChat()!
                    })
                    // Unchanged reactive captures keep their identity, so an unchanged
                    // character is returned without composing its JSON again.
                    const parts = [capturedDetail, ...capturedChats]
                    if (previous && parts.length === previousParts.length && parts.every((part, index) => part === previousParts[index])) return previous
                    previousParts = parts
                    const detailEntries: readonly (readonly [string, string])[] = capturedDetail.entries ?? Object.entries(JSON.parse(capturedDetail.json)).map(([key,value]) => [key, canonicalJson(value)] as const)
                    const details = Object.fromEntries(detailEntries
                        .map(([key,json]) => [key, decode('detail:' + key, json)]))
                    const serializedChats: string[] = []
                    const chatValues: Chat[] = []
                    for (const [index, chat] of value.chats.entries()) {
                        const captured = capturedChats[index]
                        serializedChats.push(captured.json)
                        chatValues.push(Object.fromEntries((captured.entries ?? Object.entries(JSON.parse(captured.json)).map(([key,value]) => [key,canonicalJson(value)] as const))
                            .map(([key,json]) => [key, decode('chat:' + chat.id + ':' + key, json)])) as Chat)
                    }
                    const fields = new Map(detailEntries)
                    fields.set('chats', '[' + serializedChats.join(',') + ']')
                    const order: Record<string, boolean> = {}
                    for (const key of [...fields.keys()].sort()) defineOwnEnumerableProperty(order, key, true)
                    const json = '{' + Object.keys(order).map((key) => JSON.stringify(key) + ':' + fields.get(key)).join(',') + '}'
                    if (previous?.json === json) return previous
                    return previous = {json, value: {...details, chats:chatValues} as unknown as character | groupChat}
                }
                entry = {value, capture}
                characterCaptures.set(value.chaId, entry)
            }
            result.set(value.chaId, entry.capture())
        }
        for (const id of characterCaptures.keys()) if (!result.has(id)) characterCaptures.delete(id)
        return result
    }
    const storageStringSeeds = new Map<string, readonly [string, string]>()
    const captureStorage = objectCapture(read.pluginStorage, new Set(), true, storageStringSeeds)
    const presets = fieldCapture(read.presets)
    const character = fieldCapture(read.character)
    const roots = new Map<string, { entries: ReadonlyMap<string, string>; values: ReadonlyMap<string, unknown> }>()
    return {
        characters: () => new Map([...captureCharacters()].map(([id,value]) => [id,value.json])),
        materializedCharacters: () => new Map([...captureCharacters()].map(([id,value]) => [id,value.value])),
        seedPluginStorage(capture) {
            storageStringSeeds.clear()
            for (const [key, value, json] of capture.encodedStrings ?? []) {
                storageStringSeeds.set(key, [value, json])
            }
        },
        root() {
            const captured = captureRoot()!
            if (captured.entries && !roots.has(captured.json)) {
                roots.set(captured.json, { entries: new Map(captured.entries), values: captured.values! })
                if (roots.size > 2) roots.delete(roots.keys().next().value!)
            }
            return captured.json
        },
        rootFields: (canonical) => roots.get(canonical)?.values,
        diffRoot(before, after) {
            const previous = roots.get(before)?.entries
            const current = roots.get(after)?.entries
            if (!previous || !current)
                return diffRootMutations(JSON.parse(before), JSON.parse(after))
            const result: RootMutation[] = []
            for (const key of [...new Set([...previous.keys(), ...current.keys()])].sort()) {
                if (previous.get(key) === current.get(key)) continue
                const json = current.get(key)
                result.push(
                    json === undefined
                        ? { type: 'delete', key }
                        : { type: 'set', key, value: JSON.parse(json) },
                )
            }
            return result
        },
        presets: () => {
            const result = presets()
            const json = result.volatile ? canonicalJson(read.presets()) : result.json
            return json === 'null' || json === undefined ? null : json
        },
        character: () => {
            const selected = read.character() as character | groupChat | null
            if (selected && read.characters) return captureCharacters().get(selected.chaId)?.json ?? canonicalJson(selected)
            const result = character()
            const json = result.volatile ? canonicalJson(read.character()) : result.json
            return json === 'null' || json === undefined ? null : json
        },
        pluginStorage() {
            const captured = captureStorage()
            if (!captured) return null
            return {
                get json() {
                    return captured.json
                },
                entries: captured.entries,
                get value() {
                    return JSON.parse(captured.json)
                },
            }
        },
    }
}
