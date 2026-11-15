import { sharedRootFields } from './persistentRootFields'
import { replaceArrayRange } from '../arrayRange'
import { createCatalogCharacterStub, createPresetCatalogWorkingSetFromValues, getCatalogCharacterMetadata, isCatalogPresetWorkingSet } from './workingSetCatalog'
import { createConversationSummaryStub, createConversationSummaryFromMetadata } from './conversationResidency'
import type { Chat, Database, botPreset, character } from './database.svelte'
import type { CapturedReceiveConversationEdits } from './saveCoordinator'
import { CONTENT_CHANGE_PAGE_LIMIT, type ContentChangeKey, type PersistentRevisionReader, type PersistentRoot } from './persistentDataStore'
import { canonicalClone, canonicalJson, clonePersistentRootFields } from './saveCoordinatorHelpers'
import { captureMaterializedCharacter, diffFields, diffRecordCollection, recordCollections, CHARACTER_SHARED_FIELDS, CONVERSATION_SHARED_FIELDS } from './persistentUnitCapture'
import { isWorkingSetCharacterStub, patchCatalogCharacterField } from './workingSetCatalog'
import { defineOwnEnumerableProperty } from './ownEnumerableProperty'

type CompleteCharacter = character

export class LwwWorkingSetConflictError extends Error {
    constructor(readonly kind: 'archived-owner' | 'deleted-owner' | 'deleted-conversation', readonly characterId: string, readonly conversationId?: string) {
        super(kind === 'archived-owner' ? 'Received archive conflicts with unsaved character edits' : 'Received deletion conflicts with unsaved local edits')
        this.name = 'LwwWorkingSetConflictError'
    }
}

export interface LwwWorkingSetBaseline {
    root: PersistentRoot
    presets: botPreset[] | null
    presetRecords: botPreset[]
    characters: CompleteCharacter[]
}

export function captureLwwWorkingSetBaseline(database: Database, root: PersistentRoot, presets: botPreset[] | null, materialized?: CompleteCharacter[], presetRecords: readonly botPreset[] = []): LwwWorkingSetBaseline {
    return { root: clonePersistentRootFields(root), presets: presets && canonicalClone(presets),
        presetRecords: canonicalClone([...presetRecords]),
        characters: materialized ?? database.characters.filter((value) => !isWorkingSetCharacterStub(value)).map(captureMaterializedCharacter) }
}

export function prepareLwwReceiveDiscardBaseline(baseline: LwwWorkingSetBaseline, captured: CapturedReceiveConversationEdits): { baseline: LwwWorkingSetBaseline; keys: string[] } {
    const next = canonicalClone(baseline)
    const owner = next.characters.find(value => value.chaId === captured.characterId)
    const conversation = owner?.chats.find(value => value.id === captured.conversationId)
    if (!owner || !conversation) throw new Error('Discard baseline no longer contains the selected conversation')
    const keys: string[] = []
    const fields = (target: object, prefix: string[], changes: CapturedReceiveConversationEdits['characterFields']) => {
        const record = target as Record<string, unknown>
        for (const change of changes) {
            if (change.type === 'delete') delete record[change.field]
            else defineOwnEnumerableProperty(record, change.field, canonicalClone(change.value))
            keys.push(JSON.stringify([...prefix, change.field]))
        }
    }
    fields(owner, ['character', captured.characterId], captured.characterFields)
    fields(conversation, ['conversation', captured.characterId, captured.conversationId], captured.conversationFields)
    if (captured.ranges.length) {
        if (Object.hasOwn(conversation, 'message')) {
            for (const range of captured.ranges) replaceArrayRange(conversation.message, range.start, range.deleteCount, canonicalClone(range.messages))
        }
        keys.push(JSON.stringify(['messages', captured.characterId, captured.conversationId]))
    }
    return { baseline: next, keys }
}

/** Keys needed to merge the current stored state after a receive outlives its original revision. */
export async function readLwwReceiveRebaseKeys(
    database: Database, baseline: LwwWorkingSetBaseline, reader: PersistentRevisionReader, afterRevision: number, forceCurrentState = false,
): Promise<{ keys: string[]; unknownPluginChanges: boolean }> {
    let changes: ContentChangeKey[] | null = []
    try {
        if (forceCurrentState || !reader.readWorkingSetChangePage) changes = null
        else {
            let after: ContentChangeKey | null = null
            while (true) {
                const page = await reader.readWorkingSetChangePage(afterRevision, after, CONTENT_CHANGE_PAGE_LIMIT)
                changes.push(...page)
                if (page.length < CONTENT_CHANGE_PAGE_LIMIT) break
                after = page[page.length - 1]
            }
            if (changes.some(key => !['root', 'preset', 'character', 'conversation', 'plugin', 'asset', 'inlay', 'owner'].includes(key.kind) ||
                key.kind === 'owner' && !['character-additional-assets', 'root-module-assets', 'persona-embedded-module-assets'].includes(key.key1))) changes = null
        }
    } catch { changes = null }
    const keys = new Set<string>()
    const add = (...parts: string[]) => keys.add(JSON.stringify(parts))
    const fields = (parts: string[], before: object, after: object, excluded = new Set<string>()) => {
        for (const mutation of diffFields(parts, before, after, excluded)) keys.add(mutation.key)
    }
    if (!changes || changes.some(key => key.kind === 'root' || key.kind === 'owner' && key.key1 !== 'character-additional-assets')) {
        const remote = (await reader.readRoot()).value
        for (const mutation of diffFields(['root'], baseline.root, remote)) {
            const name = (JSON.parse(mutation.key) as string[])[1]
            const before = (baseline.root as unknown as Record<string, unknown>)[name]
            const after = (remote as unknown as Record<string, unknown>)[name]
            if (recordCollections.has(name)) {
                for (const value of diffRecordCollection(name, (before ?? []) as unknown[], (after ?? []) as unknown[])) keys.add(value.key)
            } else if (name === 'personas') {
                const previous = new Map(((before ?? []) as { id: string }[]).map(value => [value.id, value]))
                const current = new Map(((after ?? []) as { id: string }[]).map(value => [value.id, value]))
                for (const id of new Set([...previous.keys(), ...current.keys()])) {
                    if (previous.has(id) !== current.has(id)) add('exists', 'persona', id)
                    if (current.has(id)) fields(['persona', id], previous.get(id) ?? {}, current.get(id)!, new Set(['id']))
                }
                if (canonicalJson([...previous.keys()]) !== canonicalJson([...current.keys()])) add('order', 'personas')
            } else if (name === 'explicitGlobalChatVariables' || name === 'protectedPresetValues') {
                for (const value of diffFields([], (before ?? {}) as object, (after ?? {}) as object)) {
                    const field = (JSON.parse(value.key) as string[])[0]
                    add(name === 'protectedPresetValues' ? 'preset-protected' : field.startsWith('toggle_') ? 'toggle' : 'variable', field)
                }
            } else keys.add(mutation.key)
        }
    }
    const liveCharacters = new Map(database.characters.map(value => [value.chaId, value]))
    const beforeCharacters = new Map(baseline.characters.map(value => [value.chaId, value]))
    const characterIds = new Set(changes?.flatMap(key => key.kind === 'character' || key.kind === 'conversation' ? [key.key1]
        : key.kind === 'owner' && key.key1 === 'character-additional-assets' ? [key.key2] : []) ?? liveCharacters.keys())
    if (!changes) {
        for (const trash of [false, true]) {
            let cursor: string | undefined
            do {
                const page = await reader.queryCharacters({ order: 'configured', trash, limit: 128, cursor })
                for (const value of page.items) characterIds.add(value.id)
                cursor = page.nextCursor
            } while (cursor)
        }
    }
    for (const id of characterIds) {
        const live = liveCharacters.get(id), before = beforeCharacters.get(id)
        const summary = await reader.readCharacterSummary(id)
        if (!summary || !live) { add('exists', 'character', id); continue }
        if (summary.archived || getCatalogCharacterMetadata(live)?.residency === 'archived') {
            add('archive', id)
            if (summary.archived) continue
        }
        if (isWorkingSetCharacterStub(live) || !before) {
            fields(['character', id], live, createCatalogCharacterStub(summary), new Set(['chaId', 'chats']))
            continue
        }
        const detail = await reader.readCharacter(id)
        if (!detail) { add('exists', 'character', id); continue }
        fields(['character', id], before, detail.value, new Set(['chaId', 'chats', 'chatFolders']))
        const currentIds: string[] = []
        let cursor: string | undefined
        do {
            const page = await reader.queryConversations({ characterId: id, order: 'configured', limit: 128, cursor })
            currentIds.push(...page.items.map(value => value.id)); cursor = page.nextCursor
        } while (cursor)
        const previous = new Map(before.chats.map(chat => [chat.id, chat]))
        const current = new Set(currentIds)
        if (canonicalJson([...previous.keys()]) !== canonicalJson(currentIds) || canonicalJson(before.chatFolders ?? []) !== canonicalJson(detail.value.chatFolders ?? [])) add('order', 'conversations', id)
        for (const chatId of new Set([...previous.keys(), ...current])) {
            if (previous.has(chatId) !== current.has(chatId)) add('exists', 'conversation', id, chatId)
            if (!current.has(chatId) || !previous.has(chatId)) continue
            const metadata = await reader.readConversationMetadata(id, chatId)
            if (!metadata) { add('exists', 'conversation', id, chatId); continue }
            fields(['conversation', id, chatId], previous.get(chatId)!, metadata.value.conversation, new Set(['id', 'message']))
            if (!changes || changes.some(key => key.kind === 'conversation' && key.key1 === id && key.key2 === chatId)) {
                const old = previous.get(chatId)!
                const current = live.chats.find(chat => chat.id === chatId)
                if (Object.hasOwn(old, 'message') && current && Object.prototype.propertyIsEnumerable.call(current, 'message')) {
                    const conversation = await reader.readConversation(id, chatId)
                    if (conversation && canonicalJson(old.message) !== canonicalJson(conversation.value.message)) add('messages', id, chatId)
                } else add('messages', id, chatId)
            }
        }
    }
    if (!changes || changes.some(key => key.kind === 'preset')) {
        const catalog = await reader.queryPresets()
        const previous = new Map([...(baseline.presets ?? []), ...baseline.presetRecords].map(value => [value['id'] as string, value]))
        const currentIds = catalog.items.map(value => value.id)
        const oldIds = database.botPresets.filter(Boolean).map(value => value['id'] as string)
        if (canonicalJson(oldIds) !== canonicalJson(currentIds)) add('order', 'presets')
        for (const id of new Set([...previous.keys(), ...currentIds])) {
            if (!previous.has(id) || !currentIds.includes(id)) { add('exists', 'preset', id); continue }
            const remote = await reader.readPreset(id)
            if (remote) fields(['preset', id], previous.get(id)!, remote.value, new Set(['id']))
        }
    }
    if (changes) {
        for (const key of changes) if (key.kind === 'plugin') add('plugin', key.key1, key.key2)
    } else {
        const owners = new Set((baseline.root.plugins ?? []).map(plugin => plugin.name))
        for (const item of (await reader.queryPluginStorage()).items) owners.add(item.owner)
        for (const owner of owners) add('order', 'plugin-storage', owner)
    }
    return { keys: [...keys], unknownPluginChanges: changes === null }
}

function patchField(live: object, baseline: object, remote: object, field: string): void {
    const current = live as Record<string, unknown>, before = baseline as Record<string, unknown>, next = remote as Record<string, unknown>
    const unchanged = canonicalJson({ value: current[field] }) === canonicalJson({ value: before[field] })
    const write = (value: Record<string, unknown>) => {
        if (!Object.hasOwn(next, field)) delete value[field]
        // Only assignment makes a state proxy report a new key to cached captures.
        else if (field === '__proto__') defineOwnEnumerableProperty(value, field, canonicalClone(next[field]))
        else value[field] = canonicalClone(next[field])
    }
    if (unchanged) write(current)
    write(before)
}

export async function applyLwwWorkingSetUnits(
    database: Database, baseline: LwwWorkingSetBaseline, store: PersistentRevisionReader, affectedKeys: readonly string[], liveIndex?: ReadonlyMap<string, CompleteCharacter>, allowLocalFields = false, beforeProjection?: () => void, afterProjection?: () => void, nativeReceive = false,
): Promise<LwwWorkingSetBaseline> {
    const charactersById = liveIndex ?? new Map(database.characters.map((value) => [value.chaId,value]))
    // A stub holds only catalog fields, so no baseline describes it.
    const baselineById = new Map(baseline.characters.filter((value) => {
        const live = charactersById.get(value.chaId)
        return !live || !isWorkingSetCharacterStub(live)
    }).map((value) => [value.chaId,value]))
    let keys = affectedKeys.map((key) => JSON.parse(key) as string[])
        .filter(([kind, , field, metadataField]) => allowLocalFields || (kind !== 'character' || CHARACTER_SHARED_FIELDS.has(field)) && (kind !== 'conversation' || CONVERSATION_SHARED_FIELDS.has(metadataField)))
        .sort((a, b) => Number(a[0] === 'root' && ['botPresetsId', 'selectedPersona'].includes(a[1])) - Number(b[0] === 'root' && ['botPresetsId', 'selectedPersona'].includes(b[1])))
    const summaries = new Map<string, Awaited<ReturnType<PersistentRevisionReader['readCharacterSummary']>>>()
    for (const [kind, id] of keys) if (nativeReceive && kind === 'archive') summaries.set(id, await store.readCharacterSummary(id))
    keys = keys.filter(([kind, id, owner]) => !summaries.get(kind === 'order' && id === 'conversations' ? owner : id)?.archived ||
        !['character', 'conversation', 'messages'].includes(kind) && !(kind === 'order' && id === 'conversations'))
    const root = keys.some(([kind]) => ['root', 'variable', 'toggle', 'record', 'order', 'persona', 'preset-protected', 'exists'].includes(kind))
        ? (await store.readRoot()).value : null
    const details = new Map<string, CompleteCharacter | null>()
    for (const [kind, id] of keys) if (kind === 'character') {
        if (!details.has(id)) {
            if (nativeReceive && charactersById.has(id) && isWorkingSetCharacterStub(charactersById.get(id)!)) {
                const summary = await store.readCharacterSummary(id)
                details.set(id, summary ? createCatalogCharacterStub(summary) : null)
            } else {
                const value = await store.readCharacter(id)
                details.set(id, value ? { ...value.value, chats: [] } as CompleteCharacter : null)
            }
        }
    }
    const presets = new Map<string, Awaited<ReturnType<PersistentRevisionReader['readPreset']>>>()
    const metadata = new Map<string, Awaited<ReturnType<PersistentRevisionReader['readConversationMetadata']>>>()
    const conversations = new Map<string, Awaited<ReturnType<PersistentRevisionReader['readConversation']>>>()
    const orders = new Map<string, string[]>()
    let presetCatalog: Awaited<ReturnType<PersistentRevisionReader['queryPresets']>> | undefined
    const pair = (characterId: string, conversationId: string) => JSON.stringify([characterId, conversationId])
    const readPreset = async (id: string) => {
        if (presets.has(id)) return
        const live = database.botPresets.find((value) => value?.['id'] === id)
        if (live && !baseline.presets?.some((value) => value['id'] === id) && !baseline.presetRecords.some((value) => value['id'] === id)) baseline.presetRecords.push(canonicalClone(live))
        presets.set(id, await store.readPreset(id))
    }
    for (const [kind, id, field, metadataField] of keys) {
        if (kind === 'preset' && database.botPresets.some((value) => value?.['id'] === id)) await readPreset(id)
        if (!isCatalogPresetWorkingSet(database.botPresets) && (kind === 'preset' || kind === 'exists' && id === 'preset')) await readPreset(kind === 'preset' ? id : field)
        if (kind === 'root' && id === 'botPresetsId' && typeof root?.botPresetsId === 'string') {
            await readPreset(root.botPresetsId)
            if (presets.get(root.botPresetsId) && !presetCatalog) presetCatalog = await store.queryPresets()
        }
        if ((kind === 'order' && id === 'presets') || (kind === 'exists' && id === 'preset')) presetCatalog ??= await store.queryPresets()
        if (kind === 'conversation' || kind === 'messages') {
            const live = charactersById.get(id)?.chats.find((value) => value.id === field)
            const before = baselineById.get(id)?.chats.find((value) => value.id === field)
            if (live && before) {
                const key = pair(id, field)
                if (kind === 'messages') {
                    if (Object.hasOwn(before, 'message') && Object.prototype.propertyIsEnumerable.call(live, 'message') && !conversations.has(key)) conversations.set(key, await store.readConversation(id, field))
                } else if (!metadata.has(key)) metadata.set(key, await store.readConversationMetadata(id, field))
            }
        }
        if (kind === 'order' && id === 'conversations' && charactersById.has(field) && baselineById.has(field) && !orders.has(field)) {
            const ids: string[] = []
            let cursor: string | undefined
            do { const page = await store.queryConversations({ characterId: field, order: 'configured', limit: 128, cursor }); ids.push(...page.items.map((item) => item.id)); cursor = page.nextCursor } while (cursor)
            orders.set(field, ids)
            if (!details.has(field)) { const value = await store.readCharacter(field); details.set(field, value ? { ...value.value, chats: [] } as CompleteCharacter : null) }
        }
        if (kind === 'exists' && id === 'character' && !summaries.has(field)) summaries.set(field, await store.readCharacterSummary(field))
        if (kind === 'exists' && id === 'conversation' && !metadata.has(pair(field, metadataField))) metadata.set(pair(field, metadataField), await store.readConversationMetadata(field, metadataField))
    }
    beforeProjection?.()
    for (const [kind, id] of keys) if (nativeReceive && kind === 'archive' && summaries.get(id)?.archived) {
        const live = charactersById.get(id), before = baselineById.get(id)
        if (live && !isWorkingSetCharacterStub(live) && (!before || canonicalJson(captureMaterializedCharacter(live)) !== canonicalJson(before))) {
            throw new LwwWorkingSetConflictError('archived-owner', id)
        }
    }
    for (const [kind, scope, characterId, conversationId] of keys) if (nativeReceive && kind === 'exists') {
        const live = charactersById.get(characterId), before = baselineById.get(characterId)
        if (!live || isWorkingSetCharacterStub(live)) continue
        if (scope === 'character' && !summaries.get(characterId) && (!before || canonicalJson(captureMaterializedCharacter(live)) !== canonicalJson(before))) {
            throw new LwwWorkingSetConflictError('deleted-owner', characterId)
        }
        if (scope === 'conversation' && !metadata.get(pair(characterId, conversationId))) {
            const current = captureMaterializedCharacter({ ...live, chats: live.chats.filter(chat => chat.id === conversationId) }).chats[0]
            const original = before?.chats.find(chat => chat.id === conversationId)
            if (current && (!original || canonicalJson(current) !== canonicalJson(original))) {
                throw new LwwWorkingSetConflictError('deleted-conversation', characterId, conversationId)
            }
        }
    }
    for (const [kind, id, field, metadataField] of keys) {
        if (kind === 'root' && root && (allowLocalFields || sharedRootFields.has(id))) {
            if (id === 'botPresetsId' && typeof root.botPresetsId === 'string') {
                const preset = presets.get(root.botPresetsId)
                if (preset) {
                    const catalog = presetCatalog!
                    const index = catalog.items.find((item) => item.id === root.botPresetsId)?.configuredIndex
                    if (index !== undefined) {
                        const live = database.botPresets.find((value) => value?.['id'] === root.botPresetsId)
                        const before = baseline.presets?.find((value) => value['id'] === root.botPresetsId) ?? baseline.presetRecords.find((value) => value['id'] === root.botPresetsId)
                        if (live && before) {
                            for (const field of new Set([...Object.keys(before), ...Object.keys(preset.value)])) patchField(live, before, preset.value, field)
                            database.botPresets[index] = live
                        } else {
                            database.botPresets[index] = canonicalClone(preset.value)
                            baseline.presetRecords.push(canonicalClone(preset.value))
                        }
                    }
                }
            }
            const remote = { ...root } as unknown as Record<string, unknown>
            if (id === 'botPresetsId' && typeof remote[id] === 'string') remote[id] = Math.max(0, database.botPresets.findIndex((value) => value?.['id'] === remote[id]))
            if (id === 'selectedPersona' && typeof remote[id] === 'string') remote[id] = Math.max(0, database.personas?.findIndex((value) => value.id === remote[id]) ?? 0)
            const before = { ...baseline.root } as unknown as Record<string, unknown>
            if (id === 'botPresetsId' && typeof before[id] === 'string') before[id] = Math.max(0, database.botPresets.findIndex((value) => value?.['id'] === before[id]))
            if (id === 'selectedPersona' && typeof before[id] === 'string') before[id] = Math.max(0, database.personas?.findIndex((value) => value.id === before[id]) ?? 0)
            patchField(database, before, remote, id)
            if (Object.hasOwn(root, id)) (baseline.root as unknown as Record<string, unknown>)[id] = canonicalClone((root as unknown as Record<string, unknown>)[id])
            else delete (baseline.root as unknown as Record<string, unknown>)[id]
            // The fallback for a received selection this working set cannot resolve is not a local selection to save.
            if (id === 'botPresetsId' || id === 'selectedPersona') {
                const records: readonly ({ id?: unknown } | undefined)[] = (id === 'botPresetsId' ? database.botPresets : database.personas) ?? []
                const received = root[id], index = database[id]
                if (typeof received === 'string' && index === remote[id] && !records.some((value) => value?.id === received)) {
                    const fallbackId = records[index]?.id
                    const baselineRoot = baseline.root as unknown as Record<string, unknown>
                    baselineRoot[id] = typeof fallbackId === 'string' ? fallbackId : index
                }
            }
        } else if (kind === 'character') {
            const live = charactersById.get(id)
            const before = baselineById.get(id)
            const remote = details.get(id)
            if (!live || !remote) continue
            // A stub takes its catalog fields from the stored record.
            if (isWorkingSetCharacterStub(live)) {
                patchCatalogCharacterField(live, remote, field)
                continue
            }
            if (!before) continue
            for (const key of [field]) {
                if (key !== 'statics' || allowLocalFields) { patchField(live, before, remote, key); continue }
                const sharedStatics = (value: unknown) => value && typeof value === 'object' ? Object.fromEntries(Object.entries(value).filter(([key]) => key !== 'messages')) : value
                const liveView = {statics:sharedStatics(live['statics'])}, beforeView = {statics:sharedStatics(before['statics'])}
                patchField(liveView, beforeView, {statics:sharedStatics(remote['statics'])}, 'statics')
                for (const [target, view] of [[live,liveView],[before,beforeView]] as const) {
                    const local = target['statics'] as Record<string, unknown> | undefined
                    if (view.statics === undefined && !local?.messages) delete target['statics']
                    else target['statics'] = {...view.statics as object, ...(local && Object.hasOwn(local, 'messages') ? {messages:local.messages} : {})}
                }
            }
        } else if (kind === 'conversation' || kind === 'messages') {
            const liveCharacter = charactersById.get(id)
            const beforeCharacter = baselineById.get(id)
            const live = liveCharacter?.chats.find((value) => value.id === field)
            const before = beforeCharacter?.chats.find((value) => value.id === field)
            if (!live || !before) continue
            if (kind === 'messages') {
                if (!Object.hasOwn(before, 'message') || !Object.prototype.propertyIsEnumerable.call(live, 'message')) continue
                const remote = conversations.get(pair(id, field))
                if (remote) patchField(live, before, remote.value, 'message')
            } else {
                const remote = metadata.get(pair(id, field))
                if (remote) patchField(live, before, remote.value.conversation, metadataField)
            }
        } else if (kind === 'preset') {
            const live = database.botPresets.find((value) => value?.['id'] === id)
            const before = baseline.presets?.find((value) => value['id'] === id) ?? baseline.presetRecords.find((value) => value['id'] === id)
            if (!live || !before) continue
            const remote = presets.get(id)
            if (remote) patchField(live, before, remote.value, field)
        } else if (root && kind === 'exists' && id === 'persona') {
            const before = baseline.root.personas ?? [], live = database.personas ?? []
            const previous = before.find((value) => value.id === field), current = live.find((value) => value.id === field), remote = root.personas?.find((value) => value.id === field)
            const replace = (values: typeof live) => remote ? [...values.filter((value) => value.id !== field), canonicalClone(remote)] : values.filter((value) => value.id !== field)
            // A deleted ID is retired, so a local edit under it could never be saved.
            if (!remote || canonicalJson(previous ?? null) === canonicalJson(current ?? null)) {
                const selectedId = live[database.selectedPersona]?.id
                database.personas = replace(live)
                const index = database.personas.findIndex((value) => value.id === selectedId)
                database.selectedPersona = Math.max(0, index)
                // The fallback after a remote deletion is not a local selection to save.
                if (index < 0) (baseline.root as unknown as Record<string, unknown>).selectedPersona = database.personas[database.selectedPersona]?.id ?? database.selectedPersona
            }
            baseline.root.personas = replace(before)
        } else if (root && kind === 'persona') {
            const live = database.personas?.find((value) => value.id === id)
            const before = baseline.root.personas?.find((value) => value.id === id)
            const remote = root.personas?.find((value) => value.id === id)
            if (live && before && remote) patchField(live, before, remote, field)
        } else if (root && (kind === 'record' || kind === 'exists' && ['modules', 'loadouts', 'customModels'].includes(id))) {
            const identity = id === 'plugins' ? 'name' : 'id'
            const live = database as unknown as Record<string, unknown[]>, before = baseline.root as unknown as Record<string, unknown[]>, remote = root as unknown as Record<string, unknown[]>
            const itemId = (value: unknown) => (value as Record<string, unknown>)[identity]
            const original = before[id]?.find((value) => itemId(value) === field)
            const current = live[id]?.find((value) => itemId(value) === field)
            const next = remote[id]?.find((value) => itemId(value) === field)
            const replace = (values: unknown[] = []) => {
                const index = values.findIndex((value) => itemId(value) === field)
                if (next === undefined) { if (index >= 0) values.splice(index, 1) }
                else if (index >= 0) values[index] = canonicalClone(next)
                else values.push(canonicalClone(next))
                return values
            }
            // A deleted ID is retired, so a local edit under it could never be saved.
            if ((kind === 'exists' && next === undefined) || canonicalJson(current ?? null) === canonicalJson(original ?? null)) live[id] = replace(live[id])
            before[id] = replace(before[id])
        } else if (root && ['variable', 'toggle', 'preset-protected'].includes(kind)) {
            const rootField = kind === 'preset-protected' ? 'protectedPresetValues' : 'explicitGlobalChatVariables'
            const live = database as unknown as Record<string, Record<string, unknown>>, before = baseline.root as unknown as Record<string, Record<string, unknown>>, remote = root as unknown as Record<string, Record<string, unknown>>
            patchField(live[rootField] ??= {}, before[rootField] ??= {}, remote[rootField] ?? {}, id)
            live[rootField] = { ...live[rootField] }
        } else if (kind === 'order' && id === 'conversations') {
            const live = charactersById.get(field)
            const before = baselineById.get(field)
            if (!live || !before) continue
            const ids = orders.get(field) ?? []
            const order = new Map(ids.map((value,index) => [value,index]))
            const sort = (chats: Chat[]) => [...chats].sort((a,b) => (order.get(a.id) ?? Infinity) - (order.get(b.id) ?? Infinity))
            if (canonicalJson(live.chats.map((value) => value.id)) === canonicalJson(before.chats.map((value) => value.id))) live.chats = sort(live.chats)
            before.chats = sort(before.chats)
            const remoteDetail = details.get(field)
            if (remoteDetail) patchField(live, before, remoteDetail, 'chatFolders')
        } else if (root && kind === 'order' && ['modules', 'plugins', 'personas', 'loadouts', 'customModels'].includes(id)) {
            const identity = id === 'plugins' ? 'name' : 'id'
            const remote = root as unknown as Record<string, Record<string, unknown>[]>
            const order = new Map((remote[id] ?? []).map((item, index) => [item[identity], index]))
            const sort = (values: Record<string, unknown>[] = []) => [...values].sort((a,b) => (order.get(a[identity]) ?? Infinity) - (order.get(b[identity]) ?? Infinity))
            const live = database as unknown as Record<string, Record<string, unknown>[]>, before = baseline.root as unknown as Record<string, Record<string, unknown>[]>
            if (canonicalJson((live[id] ?? []).map((value) => value[identity])) === canonicalJson((before[id] ?? []).map((value) => value[identity]))) {
                const selectedId = id === 'personas' ? database.personas?.[database.selectedPersona]?.id : undefined
                live[id] = sort(live[id])
                if (id === 'personas') database.selectedPersona = Math.max(0, database.personas?.findIndex((value) => value.id === selectedId) ?? 0)
            }
            before[id] = sort(before[id])
        } else if (root && kind === 'order' && id === 'characters') {
            patchField(database, baseline.root, root, 'characterOrder')
        } else if ((kind === 'order' && id === 'presets') || (kind === 'exists' && id === 'preset')) {
            const selectedId = database.botPresets[database.botPresetsId]?.['id']
            const catalog = presetCatalog!
            const byId = new Map(database.botPresets.filter(Boolean).map((value) => [value['id'], value]))
            const next = catalog.items.map((summary) => {
                const live = byId.get(summary.id)
                const catalogOnly = nativeReceive && isCatalogPresetWorkingSet(database.botPresets) && live && Object.keys(live).every(key => ['id', 'name', 'image'].includes(key))
                return !catalogOnly && live ? live : canonicalClone(presets.get(summary.id)?.value ?? { id: summary.id, name: summary.name, image: summary.image }) as botPreset
            })
            database.botPresets = isCatalogPresetWorkingSet(database.botPresets)
                ? createPresetCatalogWorkingSetFromValues(next, catalog.revision, selectedId) : next
            if (isCatalogPresetWorkingSet(database.botPresets)) for (let index = 0; index < next.length; index++) database.botPresets[index] = next[index]
            const selectedIndex = database.botPresets.findIndex((value) => value['id'] === selectedId)
            database.botPresetsId = Math.max(0, selectedIndex)
            // The fallback after a remote deletion is not a local selection to save.
            if (selectedIndex < 0) {
                const fallbackId = database.botPresets[database.botPresetsId]?.['id']
                const baselineRoot = baseline.root as unknown as Record<string, unknown>
                baselineRoot.botPresetsId = typeof fallbackId === 'string' ? fallbackId : database.botPresetsId
            }
            if (baseline.presets) baseline.presets = catalog.items.map((summary, index) => baseline.presets!.find((value) => value['id'] === summary.id) ?? canonicalClone(presets.get(summary.id)?.value ?? byId.get(summary.id) ?? next[index]))
        } else if (nativeReceive && kind === 'archive') {
            const summary = summaries.get(id)
            const index = database.characters.findIndex(value => value.chaId === id)
            if (summary && index >= 0 && (summary.archived || getCatalogCharacterMetadata(database.characters[index])?.residency === 'archived')) {
                database.characters[index] = createCatalogCharacterStub(summary)
                baseline.characters = baseline.characters.filter(value => value.chaId !== id)
            }
        } else if (kind === 'exists' && id === 'character') {
            const summary = summaries.get(field)
            if (summary && !charactersById.has(field)) database.characters.push(createCatalogCharacterStub(summary))
            if (!summary) {
                database.characters = database.characters.filter((value) => value.chaId !== field)
                baseline.characters = baseline.characters.filter((value) => value.chaId !== field)
            }
        } else if (kind === 'exists' && id === 'conversation') {
            const live = charactersById.get(field)
            const before = baselineById.get(field)
            const conversationMetadata = metadata.get(pair(field, metadataField))
            if (live && before && conversationMetadata && !live.chats.some((value) => value.id === metadataField)) {
                const stub = createConversationSummaryStub(createConversationSummaryFromMetadata(field, conversationMetadata.value.conversation, live.chats.length, conversationMetadata.value.totalMessages, 0))
                live.chats.push(stub)
                before.chats.push({ ...conversationMetadata.value.conversation } as Chat)
            }
            if (live && before && !conversationMetadata) {
                live.chats = live.chats.filter((value) => value.id !== metadataField)
                before.chats = before.chats.filter((value) => value.id !== metadataField)
            }
        }
    }
    afterProjection?.()
    return baseline
}
