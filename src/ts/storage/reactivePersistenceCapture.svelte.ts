import { isConversationSummaryStub } from './conversationResidency'
import { isTauri } from '../platform'
import { untrack } from 'svelte'
import type { Chat, character } from './database.svelte'
import type { RootMutation } from './persistentDataStore'
import {
    canonicalJson,
    requiresWholeObjectCapture,
    pluginStorageJson,
    type PluginStorageCapture,
} from './saveCoordinatorHelpers'
import { defineOwnEnumerableProperty } from './ownEnumerableProperty'
import { diffRootMutations } from './rootMutation'

/** Production-only: read closures must expose the deeply reactive DBState working set. */
export interface PersistenceCanonicalCapture {
    characterShell?(): { value: Omit<character, 'chats'> & { chats: Array<Omit<Chat, 'message'>> }; json: string } | null
    materializedCharacters?(): ReadonlyMap<string, character>
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

function completeObjectJson(value: object): string {
    const captured: Record<string, unknown> = {}
    for (const key of Object.keys(value)) {
        defineOwnEnumerableProperty(captured, key, (value as Record<string, unknown>)[key])
    }
    return canonicalJson(captured)
}

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
    let previousVolatile: { json: string; entries: null; values: undefined } | undefined
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
        const json = ordered ? pluginStorageJson(value) : canonicalJson(value)
        if (previousVolatile?.json === json) return previousVolatile
        return previousVolatile = { json, entries: null, values: undefined }
    }
}

export function createPersistenceCanonicalCapture(read: {
    root(): object
    pluginStorage(): Record<string, unknown> | null
    presets(): unknown
    character(): unknown
    characters?(): readonly (character)[]
    rootField?(key: string, value: unknown): unknown
}): PersistenceCanonicalCapture {
    const captureRoot = objectCapture(
        () => read.root() as Record<string, unknown>,
        new Set(['characters', 'botPresets', 'pluginCustomStorage', 'pluginStorageMeta', ...(isTauri ? ['account'] : [])]),
        false,
        undefined,
        read.rootField,
    )
    type CharacterCapture = { readonly json: string; value: character }
    const characterCaptures = new Map<string, { value: character; capture: () => CharacterCapture }>()
    const ownedValue = (captured: NonNullable<ReturnType<ReturnType<typeof objectCapture>>>) =>
        captured.values ? Object.fromEntries(captured.values) : JSON.parse(captured.json)
    const shellDetail = objectCapture(() => read.character() as Record<string, unknown> | null, new Set(['chats']), false)
    const shellChats = new Map<Chat, ReturnType<typeof objectCapture>>()
    let previousShellParts: unknown[] = []
    let previousShell: NonNullable<ReturnType<NonNullable<PersistenceCanonicalCapture['characterShell']>>> | null = null
    const captureShell = () => {
        const character = read.character() as character | null
        if (character && requiresWholeObjectCapture(character)) {
            const captured = JSON.parse(completeObjectJson(character)) as character
            const value = { ...captured, chats: captured.chats.map(({ message: _message, ...chat }) => chat) }
            const json = canonicalJson(value)
            if (previousShell?.json === json) return previousShell
            previousShellParts = []
            return previousShell = { value, json }
        }
        const detail = shellDetail()
        if (!character || !detail) { shellChats.clear(); previousShell = null; return null }
        const active = new Set(character.chats)
        for (const chat of shellChats.keys()) if (!active.has(chat)) shellChats.delete(chat)
        const chats = character.chats.map((chat) => {
            let capture = shellChats.get(chat)
            if (!capture) {
                const fields = untrack(() => objectCapture(() => chat as unknown as Record<string, unknown>, new Set(['message']), false))
                let previous: ReturnType<typeof fields>
                capture = () => {
                    if (!requiresWholeObjectCapture(chat, new Set(['message']))) return fields()
                    const { message: _message, ...metadata } = JSON.parse(completeObjectJson(chat)) as Chat
                    const json = canonicalJson(metadata)
                    if (previous?.json === json) return previous
                    return previous = { json, entries: null, values: undefined }
                }
                shellChats.set(chat, capture)
            }
            return capture()!
        })
        const parts = [detail, ...chats]
        if (previousShell && parts.length === previousShellParts.length && parts.every((part, index) => part === previousShellParts[index])) return previousShell
        previousShellParts = parts
        const value = { ...ownedValue(detail), chats: chats.map(ownedValue) } as NonNullable<typeof previousShell>['value']
        let json: string | undefined
        return previousShell = { value, get json() { return json ??= canonicalJson(value) } }
    }
    const captureCharacters = () => {
        const result = new Map<string, CharacterCapture>()
        for (const value of read.characters?.() ?? []) {
            let entry = characterCaptures.get(value.chaId)
            if (!entry || entry.value !== value) {
                const detail = untrack(() => objectCapture(() => value as unknown as Record<string, unknown>, new Set(['chats']), false))
                const chats = new Map<Chat, () => Chat>()
                let previous: CharacterCapture | undefined
                let previousDetail: unknown
                const capture = () => {
                    if (requiresWholeObjectCapture(value)) {
                        previousDetail = undefined
                        const json = completeObjectJson(value)
                        if (previous?.json === json) return previous
                        return previous = { json, value: JSON.parse(json) }
                    }
                    const capturedDetail = detail()!
                    const active = new Set(value.chats)
                    for (const chat of chats.keys()) if (!active.has(chat)) chats.delete(chat)
                    const chatValues = value.chats.map((chat) => {
                        let captureChat = chats.get(chat)
                        if (!captureChat) {
                            const metadata = untrack(() => objectCapture(() => chat as unknown as Record<string, unknown>, new Set(['message']), false))
                            const messages = new Map<object, () => Chat['message'][number]>()
                            const messageHooks = new Map<object, { json: string; value: Chat['message'][number] }>()
                            let previousMetadata: unknown
                            let previousChat: Chat | undefined
                            let previousMessages: Chat['message'] | undefined
                            let previousWholeJson: string | undefined
                            captureChat = () => {
                                const complete = !isConversationSummaryStub(chat) && Object.prototype.propertyIsEnumerable.call(chat, 'message')
                                if (requiresWholeObjectCapture(chat, complete ? undefined : new Set(['message']))) {
                                    const json = completeObjectJson(chat)
                                    if (previousWholeJson === json) return previousChat!
                                    previousWholeJson = json
                                    previousMetadata = undefined
                                    previousMessages = undefined
                                    return previousChat = JSON.parse(json)
                                }
                                previousWholeJson = undefined
                                const capturedMetadata = metadata()!
                                let nextMessages: Chat['message'] | undefined
                                if (!isConversationSummaryStub(chat) && Object.prototype.propertyIsEnumerable.call(chat, 'message')) {
                                    const body = chat.message
                                    const activeMessages = new Set<object>(body)
                                    for (const message of messages.keys()) if (!activeMessages.has(message)) messages.delete(message)
                                    for (const message of messageHooks.keys()) if (!activeMessages.has(message)) messageHooks.delete(message)
                                    const values = body.map((message, index) => {
                                        if (requiresWholeObjectCapture(message)) {
                                            const json = canonicalJson({ [index]: message })
                                            let captured = messageHooks.get(message)
                                            if (captured?.json !== json) {
                                                captured = { json, value: JSON.parse(json)[index] }
                                                messageHooks.set(message, captured)
                                            }
                                            return captured.value
                                        }
                                        let captureMessage = messages.get(message)
                                        if (!captureMessage) {
                                            const fields = untrack(() => objectCapture(() => message as unknown as Record<string, unknown>, new Set(), false))
                                            let previousFields: unknown
                                            let previousValue: Chat['message'][number]
                                            captureMessage = () => {
                                                const captured = fields()!
                                                if (captured !== previousFields) {
                                                    previousFields = captured
                                                    previousValue = ownedValue(captured)
                                                }
                                                return previousValue
                                            }
                                            messages.set(message, captureMessage)
                                        }
                                        return captureMessage()
                                    })
                                    nextMessages = previousMessages && values.length === previousMessages.length &&
                                        values.every((message, index) => message === previousMessages![index]) ? previousMessages : values
                                }
                                if (previousChat && previousMetadata === capturedMetadata && previousMessages === nextMessages) return previousChat
                                previousMetadata = capturedMetadata
                                previousMessages = nextMessages
                                previousChat = { ...ownedValue(capturedMetadata), ...(nextMessages ? { message: nextMessages } : {}) } as Chat
                                return previousChat
                            }
                            chats.set(chat, captureChat)
                        }
                        return captureChat()
                    })
                    if (previous && previousDetail === capturedDetail && chatValues.length === previous.value.chats.length &&
                        chatValues.every((chat, index) => chat === previous!.value.chats[index])) return previous
                    previousDetail = capturedDetail
                    const owned = { ...ownedValue(capturedDetail), chats: chatValues } as character
                    let json: string | undefined
                    return previous = { value: owned, get json() { return json ??= canonicalJson(owned) } }
                }
                entry = { value, capture }
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
        characterShell: captureShell,
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
            const selected = read.character() as character | null
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
