import type { PluginStorageValueCursor } from "../storage/persistentDataStore"
import { reconcilePluginListUpdate } from './pluginListUpdate'
import type { RisuPlugin } from "./plugins.svelte"
import type { Chat, Database } from '../storage/database.svelte'
import type {
    CompleteConversationLease,
    SelectedConversationTarget,
} from '../storage/activeWorkingSet.svelte'
import type { ActiveConversationSession } from '../storage/activeConversationSession'
import { getPersistentDataStore } from '../storage/persistentDataStoreFactory'
import { isWorkingSetCharacterStub } from '../storage/workingSetCatalog'
import type {
    CharacterPage,
    ConversationPage,
    ConversationWindow,
    DataRevision,
    PersistentDataStore,
    PersistentRevisionLease,
    PluginStorageValue,
    ConversationMutation,
} from '../storage/persistentDataStore'
import {
    acquireCurrentRevisionWithRetry,
    assertPinnedRevision,
    iterateUnarchivedPinnedCharacterSummaries,
    iteratePinnedCharacters,
    iteratePinnedConversations,
    releasePersistentRevisionLease,
    withPersistentRevisionLease,
} from '../storage/persistentRecordIterator'
import { defineOwnEnumerableProperty } from '../storage/ownEnumerableProperty'
import { isConversationSummaryStub } from '../storage/conversationResidency'
import { resolveLifecyclePluginStorageOwner } from './pluginStorageStore'
import type { PluginStorageMeta } from './pluginOwner'
import { attachPluginReadProvenance, PluginReadBaselineError, PluginReadBaselines, collectPluginReadProvenance, type PluginReadProvenance } from './pluginReadBaselines'
import { pluginUnitIntents, type PluginUnitMutation, type PluginWholeMessageIntent } from './pluginUnitIntents'
import {
    readPinnedConversationContext,
    type ConversationContext,
    type ConversationContextRequest,
} from './conversationContext'
import {
    PLUGIN_SUMMARY_QUERY_DEFAULT_LIMIT,
    PLUGIN_SUMMARY_QUERY_MAX_LIMIT,
    PLUGIN_MESSAGE_QUERY_MAX_LIMIT,
    pluginMessageWindow,
    positiveLimit,
    requiredId,
} from './pluginQueryInput'

export interface PluginCharacterQuery {
    search?: string
    order?: 'configured' | 'recent'
    trash?: boolean
    limit?: number
    cursor?: string
}

export interface PluginConversationQuery {
    characterId: string
    order?: 'configured' | 'recent'
    limit?: number
    cursor?: string
}

export interface PluginConversationMessageQuery {
    characterId: string
    conversationId: string
    startIndex?: number
    limit?: number
    anchorMessageId?: string
    before?: number
    after?: number
    signal?: AbortSignal
}

export interface PluginConversationWindow extends ConversationWindow {
    revision: DataRevision
}

export type PluginCompleteCharacter = Database['characters'][number]

export interface PluginChatOutputProjectionInput {
    characterId: string
    conversationId: string
    liveCharacter: PluginCompleteCharacter
    liveConversation: Chat
}

export interface PluginChatOutputProjection {
    char: PluginCompleteCharacter
    chat: Chat
}

export type PluginChatOutputProjector = (
    input: PluginChatOutputProjectionInput,
) => Promise<PluginChatOutputProjection>

export interface PluginFullObjectCallContext {
    pluginName: string
    signal: AbortSignal
}

export interface PluginResolvedCharacterTarget {
    revision: DataRevision
    characterId: string
}

export interface PluginResolvedConversationTarget extends PluginResolvedCharacterTarget {
    conversationId: string
}

export type PluginIdentityReplacementOperation =
    | 'setCharacter'
    | 'setCharacterToIndex'
    | 'setChatToIndex'

export interface PluginIdentityReplacementDiagnostic {
    kind: 'plugin-identity-replacement-rejected'
    pluginName: string
    operation: PluginIdentityReplacementOperation
    targetId: string
    attemptedId: string
}

export class PluginIdentityReplacementRejectedError extends Error {
    readonly diagnostic: PluginIdentityReplacementDiagnostic

    constructor(diagnostic: PluginIdentityReplacementDiagnostic) {
        super(`${diagnostic.operation} cannot replace identity ${diagnostic.targetId}`)
        this.name = 'PluginIdentityReplacementRejectedError'
        this.diagnostic = diagnostic
    }
}

export class PluginFullObjectTargetStaleError extends Error {
    constructor(characterId: string, conversationId?: string) {
        super(
            conversationId
                ? `Plugin full-object target became stale: ${characterId}/${conversationId}`
                : `Plugin full-object target became stale: ${characterId}`,
        )
        this.name = 'PluginFullObjectTargetStaleError'
    }
}

export interface PluginDatabaseAccessDependencies {
    /** The plugin every call in this instance is confined to. */
    owner: string
    store: PersistentDataStore
    getPersistentRevision(): DataRevision
    commitPersistentUnitIntent(
        reason: string,
        units: readonly PluginUnitMutation[],
        conversations?: readonly ConversationMutation[],
        wholeMessages?: readonly PluginWholeMessageIntent[],
    ): Promise<void>
    flushPendingData(reason: string): Promise<void>
    getCompatibilityDatabase(): Database
    getSelectedCharacterId(): string | null
    captureSelectedConversationTarget(): SelectedConversationTarget | null
    acquireCompleteConversation(
        reason: string,
        target?: SelectedConversationTarget | null,
    ): Promise<CompleteConversationLease>
    refreshSelectedConversationAfterReplacement(
        target: SelectedConversationTarget,
        expectedSession: ActiveConversationSession,
    ): boolean
    invalidateActiveConversationSession?(): void
    reportIdentityReplacementRejected(
        diagnostic: PluginIdentityReplacementDiagnostic,
    ): void
    getNavigationGeneration(): number
    getStorageAuthorityEpoch(): number
    assertPersistentMutationAllowed(expectedAuthorityEpoch?: number): void
    readPluginStorageSnapshot(): Promise<Record<string, unknown>>
    prepareAuthoritativeDatabaseUpdate?(
        database: Record<string, unknown>,
    ): Promise<Record<string, unknown>>
    snapshot<T>(value: T): T
}

export interface PluginDatabaseAccess {
    expireReadBaselines(): void
    closeReadBaselines(): void
    getFullObjectSnapshotStream(
        target: { characterIndex?: number; chatIndex?: number },
        context: PluginFullObjectCallContext,
    ): Promise<PluginIframeSnapshot | null | undefined>
    getCurrentCharacter(
        context: PluginFullObjectCallContext,
    ): Promise<PluginCompleteCharacter | undefined>
    getCharacterFromIndex(
        index: number,
        context: PluginFullObjectCallContext,
    ): Promise<PluginCompleteCharacter | null>
    getChatFromIndex(
        characterIndex: number,
        chatIndex: number,
        context: PluginFullObjectCallContext,
    ): Promise<Chat | null>
    setCurrentCharacter(
        character: PluginCompleteCharacter,
        context: PluginFullObjectCallContext,
    ): Promise<void>
    setCharacterToIndex(
        index: number,
        character: PluginCompleteCharacter,
        context: PluginFullObjectCallContext,
    ): Promise<void>
    setChatToIndex(
        characterIndex: number,
        chatIndex: number,
        chat: Chat,
        context: PluginFullObjectCallContext,
    ): Promise<void>
    queryCharacters(input?: PluginCharacterQuery): Promise<CharacterPage>
    queryConversations(input: PluginConversationQuery): Promise<ConversationPage>
    queryConversationMessages(
        input: PluginConversationMessageQuery,
    ): Promise<PluginConversationWindow | null>
    readConversationContext(
        request: ConversationContextRequest,
        options: { allowPrivate: boolean; signal?: AbortSignal },
    ): Promise<ConversationContext | null>
    getDatabaseSnapshot(
        includeOnly: string[] | 'all',
        allowedKeys: readonly string[],
    ): Promise<Record<string, unknown>>
    getDatabaseSnapshotStream(
        includeOnly: string[] | 'all',
        allowedKeys: readonly string[],
    ): Promise<ReadableStream<PluginDatabaseSnapshotChunk>>
    setDatabaseLite(
        database: Record<string, unknown>,
        allowedKeys: readonly string[],
    ): void | Promise<void>
    setDatabase(
        database: Record<string, unknown>,
        allowedKeys: readonly string[],
    ): Promise<void>
}

export interface PluginIframeSnapshot {
    __type: 'IFRAME_OBJECT_STREAM'
    value: ReadableStream<PluginDatabaseSnapshotChunk>
    select: 'character' | 'conversation'
}

export type PluginDatabaseSnapshotChunk =
    | { type: 'provenance'; entries: PluginReadProvenance[] }
    | { type: 'set'; key: string; value: unknown }
    | { type: 'arrayStart'; key: string }
    | { type: 'arrayPush'; key: string; value: unknown }
    | { type: 'recordStart'; key: string }
    | { type: 'recordSet'; key: string; entryKey: string; value: unknown }
    | { type: 'characterStart'; key: 'characters'; value: unknown }
    | { type: 'conversationStart'; key: 'characters'; value: unknown }
    | { type: 'message'; key: 'characters'; value: unknown }

function accumulatePluginSnapshotChunk(result: Record<string, any>, chunk: PluginDatabaseSnapshotChunk): void {
    if (chunk.type === 'provenance') return
    if (chunk.type === 'set') defineOwnEnumerableProperty(result, chunk.key, structuredClone(chunk.value))
    else if (chunk.type === 'arrayStart') defineOwnEnumerableProperty(result, chunk.key, [])
    else if (chunk.type === 'arrayPush') result[chunk.key].push(structuredClone(chunk.value))
    else if (chunk.type === 'recordStart') defineOwnEnumerableProperty(result, chunk.key, {})
    else if (chunk.type === 'recordSet') defineOwnEnumerableProperty(result[chunk.key], chunk.entryKey, structuredClone(chunk.value))
    else if (chunk.type === 'characterStart') result.characters.push({ ...structuredClone(chunk.value as object), chats: [] })
    else if (chunk.type === 'conversationStart') result.characters.at(-1).chats.push({ ...structuredClone(chunk.value as object), message: [] })
    else if (chunk.type === 'message') result.characters.at(-1).chats.at(-1).message.push(structuredClone(chunk.value))
}

async function* streamPinnedPluginValues(reader: PersistentRevisionLease, owner: string): AsyncGenerator<PluginStorageValue> {
    let afterKey: PluginStorageValueCursor | undefined
    do {
        const page = await reader.readPluginStorageValues({ owner, afterKey })
        assertPinnedRevision(reader.revision, page.revision, 'Plugin storage values')
        for (const item of page.items) {
            if (item.owner !== owner) throw new Error('Plugin storage page contains a foreign owner')
            yield item
        }
        afterKey = page.nextCursor ?? undefined
    } while (afterKey !== undefined)
}

async function* streamPinnedConversation(
    reader: PersistentRevisionLease,
    characterId: string,
    conversationId: string,
): AsyncGenerator<PluginDatabaseSnapshotChunk> {
    const metadata = await reader.readConversationMetadata(characterId, conversationId)
    if (!metadata) throw new Error(`Missing conversation ${conversationId}`)
    assertPinnedRevision(reader.revision, metadata.revision, 'Conversation metadata')
    yield { type: 'conversationStart', key: 'characters', value: metadata.value.conversation }
    for (let startIndex = 0; startIndex < metadata.value.totalMessages; startIndex += PLUGIN_MESSAGE_QUERY_MAX_LIMIT) {
        const page = await reader.readConversationWindow({
            characterId, conversationId, startIndex, limit: PLUGIN_MESSAGE_QUERY_MAX_LIMIT,
        })
        if (!page) throw new Error(`Missing conversation ${conversationId}`)
        assertPinnedRevision(reader.revision, page.revision, 'Conversation messages')
        if (page.value.startIndex !== startIndex || page.value.messages.length !== Math.min(PLUGIN_MESSAGE_QUERY_MAX_LIMIT, metadata.value.totalMessages - startIndex)
            || page.value.totalMessages !== metadata.value.totalMessages) {
            throw new Error('Incomplete plugin snapshot message page')
        }
        for (const message of page.value.messages) {
            yield { type: 'message', key: 'characters', value: message }
        }
    }
}

async function* streamPinnedCharacter(
    reader: PersistentRevisionLease,
    characterId: string,
    detail: unknown,
): AsyncGenerator<PluginDatabaseSnapshotChunk> {
    yield { type: 'characterStart', key: 'characters', value: detail }
    let cursor: string | undefined
    do {
        const page = await reader.queryConversations({
            characterId, order: 'configured', limit: 100, cursor,
        })
        assertPinnedRevision(reader.revision, page.revision, 'Conversation catalog')
        for (const summary of page.items) {
            yield* streamPinnedConversation(reader, characterId, summary.id)
        }
        cursor = page.nextCursor
    } while (cursor !== undefined)
}

const SYNCHRONOUS_CHARACTER_SET_ERROR =
    'Synchronous plugin character updates are unavailable. Use async setDatabase().'
const STALE_DATABASE_SET_ERROR =
    'Plugin database update became stale because navigation state changed.'
const DANGEROUS_DATABASE_KEYS = new Set(['__proto__', 'prototype', 'constructor'])

export function linkPluginQueryAbortSignals(
    ...signals: Array<AbortSignal | undefined>
): {
    signal: AbortSignal
    dispose(): void
} {
    const activeSignals = signals.filter((signal): signal is AbortSignal => Boolean(signal))
    if (activeSignals.length === 1) {
        return { signal: activeSignals[0], dispose() {} }
    }

    const controller = new AbortController()
    const listeners = activeSignals.map((signal) => {
        const listener = () => controller.abort(signal.reason)
        if (signal.aborted) listener()
        else signal.addEventListener('abort', listener, { once: true })
        return { signal, listener }
    })
    return {
        signal: controller.signal,
        dispose() {
            for (const { signal, listener } of listeners) {
                signal.removeEventListener('abort', listener)
            }
        },
    }
}

function throwIfQueryAborted(signal: AbortSignal | undefined): void {
    if (!signal?.aborted) return
    throw signal.reason ?? new DOMException('The operation was aborted', 'AbortError')
}

function throwIfFullObjectCallAborted(signal: AbortSignal): void {
    if (!signal.aborted) return
    throw signal.reason ?? new DOMException('The operation was aborted', 'AbortError')
}

async function resolvePinnedCharacterTarget(
    reader: PersistentRevisionLease,
    index: number,
): Promise<PluginResolvedCharacterTarget | null> {
    if (!Number.isSafeInteger(index) || index < 0) return null
    let position = 0
    // The position API walks the same filtered sequence the full database read
    // walks, so an index means the same character in both.
    for await (const summary of iterateUnarchivedPinnedCharacterSummaries(reader)) {
        if (position++ === index) {
            return { revision: reader.revision, characterId: summary.id }
        }
    }
    return null
}

async function resolvePinnedConversationTarget(
    reader: PersistentRevisionLease,
    characterIndex: number,
    chatIndex: number,
): Promise<PluginResolvedConversationTarget | null> {
    if (!Number.isSafeInteger(chatIndex) || chatIndex < 0) return null
    const character = await resolvePinnedCharacterTarget(reader, characterIndex)
    if (!character) return null
    let position = 0
    let cursor: string | undefined
    do {
        const page = await reader.queryConversations({
            characterId: character.characterId,
            order: 'configured',
            limit: 128,
            cursor,
        })
        assertPinnedRevision(
            reader.revision,
            page.revision,
            `Conversation page for ${character.characterId}`,
        )
        for (const summary of page.items) {
            if (position++ === chatIndex) {
                return { ...character, conversationId: summary.id }
            }
        }
        cursor = page.nextCursor
    } while (cursor !== undefined)
    return null
}

async function readPinnedCompleteCharacter(
    reader: PersistentRevisionLease,
    characterId: string,
): Promise<PluginCompleteCharacter | null> {
    const detail = await reader.readCharacter(characterId)
    if (!detail) return null
    assertPinnedRevision(reader.revision, detail.revision, `Character ${characterId}`)
    const chats: Chat[] = []
    for await (const conversation of iteratePinnedConversations(reader, characterId)) {
        chats.push(conversation.value)
    }
    return { ...detail.value, chats } as PluginCompleteCharacter
}

function hasCharacterUpdate(database: Record<string, unknown>): boolean {
    return Object.prototype.hasOwnProperty.call(database, 'characters')
}

function isPlainRecord(value: unknown): value is Record<string, unknown> {
    if (value === null || typeof value !== 'object') return false
    const prototype = Object.getPrototypeOf(value)
    return prototype === Object.prototype || prototype === null
}

function validateSafeKeys(database: Record<string, unknown>): void {
    for (const key of Object.keys(database)) {
        if (DANGEROUS_DATABASE_KEYS.has(key)) {
            throw new TypeError(`Unsafe plugin database key: ${key}`)
        }
    }
}

export function validatePluginDatabaseUpdate(
    database: unknown,
): asserts database is Record<string, unknown> {
    if (!isPlainRecord(database)) {
        throw new TypeError('Plugin database update must be a plain record')
    }
    validateSafeKeys(database)
    if (Object.prototype.hasOwnProperty.call(database, 'plugins')) {
        if (!Array.isArray(database.plugins) || database.plugins.some((plugin) =>
            !isPlainRecord(plugin) || typeof plugin.name !== 'string' || typeof plugin.script !== 'string')) {
            throw new TypeError('plugins must be an array of plugin records')
        }
    }
    if (Object.prototype.hasOwnProperty.call(database, 'pluginCustomStorage')) {
        if (!isPlainRecord(database.pluginCustomStorage)) {
            throw new TypeError('pluginCustomStorage must be a plain record')
        }
        validateSafeKeys(database.pluginCustomStorage)
    }
}

function validatePluginCompleteChat(value: unknown): asserts value is Chat {
    if (!value || typeof value !== 'object') {
        throw new TypeError('Plugin conversation replacement must be a complete object')
    }
    const record = value as Record<string, unknown>
    if (typeof record.id !== 'string' || record.id.length === 0) {
        throw new TypeError('Plugin conversation replacement must have a nonempty ID')
    }
    if (!Array.isArray(record.message)) {
        throw new TypeError('Plugin conversation replacement messages must be an array')
    }
}

function validatePluginCompleteCharacter(
    value: unknown,
): asserts value is PluginCompleteCharacter {
    if (!value || typeof value !== 'object') {
        throw new TypeError('Plugin character replacement must be a complete object')
    }
    const record = value as Record<string, unknown>
    if (typeof record.chaId !== 'string' || record.chaId.length === 0) {
        throw new TypeError('Plugin character replacement must have a nonempty character ID')
    }
    if (isWorkingSetCharacterStub(value as PluginCompleteCharacter)) {
        throw new TypeError('Plugin database characters cannot contain catalog working-set stubs')
    }
    if (!Array.isArray(record.chats)) {
        throw new TypeError(`Plugin database character ${record.chaId} is not fully hydrated`)
    }
    const conversationIds = new Set<string>()
    for (const conversation of record.chats) {
        if (
            !conversation ||
            typeof conversation !== 'object' ||
            typeof (conversation as Record<string, unknown>).id !== 'string' ||
            (conversation as Record<string, unknown>).id === '' ||
            !Array.isArray((conversation as Record<string, unknown>).message)
        ) {
            throw new TypeError(`Plugin database character ${record.chaId} is not fully hydrated`)
        }
        if (conversationIds.has(conversation.id!)) {
            throw new TypeError(`Plugin character contains duplicate conversation ID ${conversation.id}`)
        }
        conversationIds.add(conversation.id!)
    }
}

function validateCompleteCharacters(value: unknown): asserts value is Database['characters'] {
    if (!Array.isArray(value)) {
        throw new TypeError('Plugin database characters must be an array')
    }
    const characterIds = new Set<string>()
    for (const character of value) {
        validatePluginCompleteCharacter(character)
        if (characterIds.has(character.chaId)) {
            throw new TypeError(`Plugin database contains duplicate character ID ${character.chaId}`)
        }
        characterIds.add(character.chaId)
    }
}

/**
 * A full replacement writes the flat projection back, so the ownership sidecar
 * has to ride along or every row would land unowned. Keys the calling plugin
 * supplied belong to it; the rest keep the owner the store already records. A
 * plugin that sends an explicit `pluginCustomStorage` replaces its own keys
 * only, because the snapshot it read never showed it anyone else's.
 */
export function applyPluginDatabaseUpdate(
    candidate: Database,
    update: Record<string, unknown>,
    allowedKeys: readonly string[],
    owner: string,
    ownerOf: (key: string) => string = resolveLifecyclePluginStorageOwner,
): void {
    validatePluginDatabaseUpdate(update)
    const mutableCandidate = candidate as unknown as Record<string, unknown>
    const allowedKeySet = new Set(allowedKeys)
    const hasExplicitCustomStorage = Object.prototype.hasOwnProperty.call(
        update,
        'pluginCustomStorage',
    ) && allowedKeySet.has('pluginCustomStorage')
    const existingCustomStorage = candidate.pluginCustomStorage ?? {}
    if (!isPlainRecord(existingCustomStorage)) {
        throw new TypeError('Existing pluginCustomStorage must be a plain record')
    }
    const customStorage: Record<string, unknown> = {}
    for (const [key, value] of Object.entries(existingCustomStorage)) {
        if (hasExplicitCustomStorage && ownerOf(key) === owner) continue
        customStorage[key] = value
    }
    if (hasExplicitCustomStorage) {
        Object.assign(customStorage, update.pluginCustomStorage as Record<string, unknown>)
    }

    for (const key of Object.keys(update).filter((key) => allowedKeySet.has(key)).sort()) {
        if (key === 'plugins') {
            const reconciled = reconcilePluginListUpdate(candidate.plugins ?? [], update.plugins as RisuPlugin[])
            candidate.plugins = [...reconciled.installed, ...reconciled.additions]
        } else if (key !== 'pluginCustomStorage') mutableCandidate[key] = update[key]
    }
    const updatedKeys = new Set<string>()
    for (const key of Object.keys(update).filter((key) => !allowedKeySet.has(key)).sort()) {
        customStorage[key] = update[key]
        updatedKeys.add(key)
    }
    if (hasExplicitCustomStorage) {
        for (const key of Object.keys(update.pluginCustomStorage as Record<string, unknown>)) {
            updatedKeys.add(key)
        }
    }
    candidate.pluginCustomStorage = customStorage
    const meta: PluginStorageMeta = {}
    const now = Date.now()
    for (const key of Object.keys(customStorage)) {
        meta[key] = { plugin: updatedKeys.has(key) ? owner : ownerOf(key), updatedAt: now }
    }
    ;(candidate as Database & { pluginStorageMeta?: PluginStorageMeta }).pluginStorageMeta = meta
}

export function createPluginDatabaseAccess(
    dependencies: PluginDatabaseAccessDependencies,
): PluginDatabaseAccess {
    const readBaselines = new PluginReadBaselines(dependencies.owner, dependencies.getStorageAuthorityEpoch)
    const admissionDatabase = () => dependencies.snapshot(dependencies.getCompatibilityDatabase())
    const snapshotWithProvenance = <T>(value: T): T => {
        const detached = dependencies.snapshot(value)
        attachPluginReadProvenance(detached, collectPluginReadProvenance(value))
        return detached
    }
    const databaseUnitChanges = (intents: ReturnType<PluginReadBaselines['intent']>, submitted: Record<string, unknown>, allowedKeys: readonly string[]) => {
        const normalized = intents.map(intent => allowedKeys.includes(String(intent.path[0])) || intent.path[0] === 'pluginCustomStorage'
            ? intent
            : { ...intent, path: ['pluginCustomStorage', ...intent.path] })
        return pluginUnitIntents(normalized, 'database', dependencies.owner, submitted)
    }
    const captureCharacterAdmission = (characterId: string, characterIndex?: number): Promise<unknown> => {
        const live = dependencies.getCompatibilityDatabase().characters?.find(value => value.chaId === characterId)
        const hydratedDetail = Boolean(live && !isWorkingSetCharacterStub(live))
        const hydratedChats = new Set(live?.chats.filter(chat => !isConversationSummaryStub(chat)).map(chat => chat.id))
        const admission = live ? dependencies.snapshot(live) : undefined
        if (admission && hydratedDetail && admission.chats.every(chat => hydratedChats.has(chat.id))) return Promise.resolve(admission)
        const revision = dependencies.getPersistentRevision()
        const authority = dependencies.getStorageAuthorityEpoch()
        const pin = dependencies.store.acquireRevision(revision)
        return pin.then(reader => withPersistentRevisionLease(reader, async reader => {
            assertPinnedRevision(revision, reader.revision, 'Plugin admission')
            dependencies.assertPersistentMutationAllowed(authority)
            const target = characterIndex === undefined ? { characterId } : await resolvePinnedCharacterTarget(reader, characterIndex)
            if (!target) return {}
            const persisted = await readPinnedCompleteCharacter(reader, target.characterId)
            dependencies.assertPersistentMutationAllowed(authority)
            if (!persisted) throw new PluginReadBaselineError('target')
            if (!admission || !hydratedDetail) return dependencies.snapshot(persisted)
            const { chats, ...detail } = admission
            return {
                ...dependencies.snapshot(persisted), ...detail,
                chats: persisted.chats.map(chat => {
                    const captured = chats.find(value => value.id === chat.id)
                    return captured && hydratedChats.has(captured.id) ? captured : dependencies.snapshot(chat)
                }),
            }
        })).catch(error => {
            if (error instanceof PluginReadBaselineError) throw error
            throw new PluginReadBaselineError('stale')
        })
    }
    const captureDatabaseAdmission = (submitted: Record<string, unknown>): Promise<Record<string, unknown>> => {
        const liveHydration = new Map(dependencies.getCompatibilityDatabase().characters?.map(character => [character.chaId, { detail: !isWorkingSetCharacterStub(character), chats: new Set(character.chats.filter(chat => !isConversationSummaryStub(chat)).map(chat => chat.id)) }]))
        const captured = admissionDatabase() as unknown as Record<string, any>
        const needsPinned = ['characters', 'botPresets', 'pluginCustomStorage'].some(key => Object.hasOwn(submitted, key))
        if (!needsPinned) return Promise.resolve(captured)
        const revision = dependencies.getPersistentRevision()
        const authority = dependencies.getStorageAuthorityEpoch()
        const pin = dependencies.store.acquireRevision(revision)
        return pin.then(reader => withPersistentRevisionLease(reader, async reader => {
            assertPinnedRevision(revision, reader.revision, 'Plugin database admission')
            dependencies.assertPersistentMutationAllowed(authority)
            if (Object.hasOwn(submitted, 'characters')) {
                const characters: PluginCompleteCharacter[] = []
                for await (const item of iteratePinnedCharacters(reader)) {
                    const character = await readPinnedCompleteCharacter(reader, item.summary.id)
                    if (!character) throw new PluginReadBaselineError('target')
                    const live = (captured.characters as PluginCompleteCharacter[] | undefined)?.find(value => value.chaId === item.summary.id)
                    const hydration = live && liveHydration.get(live.chaId)
                    const complete = dependencies.snapshot(character)
                    if (live && hydration) {
                        const chats = complete.chats.map(chat => hydration.chats.has(chat.id) ? live.chats.find(value => value.id === chat.id)! : chat)
                        characters.push(hydration.detail ? { ...live, chats } : { ...complete, chats })
                    } else characters.push(complete)
                }
                captured.characters = characters
            }
            if (Object.hasOwn(submitted, 'botPresets')) {
                const catalog = await reader.queryPresets()
                assertPinnedRevision(revision, catalog.revision, 'Plugin preset admission')
                const presets: unknown[] = []
                for (const item of catalog.items) {
                    const preset = await reader.readPreset(item.id)
                    if (!preset) throw new PluginReadBaselineError('target')
                    assertPinnedRevision(revision, preset.revision, 'Plugin preset admission')
                    presets.push(dependencies.snapshot(preset.value))
                }
                captured.botPresets = presets
            }
            if (Object.hasOwn(submitted, 'pluginCustomStorage')) {
                const storage: Record<string, unknown> = {}
                for await (const item of streamPinnedPluginValues(reader, dependencies.owner)) defineOwnEnumerableProperty(storage, item.key, dependencies.snapshot(item.value))
                captured.pluginCustomStorage = storage
            }
            dependencies.assertPersistentMutationAllowed(authority)
            return captured
        })).catch(error => {
            if (error instanceof PluginReadBaselineError) throw error
            throw new PluginReadBaselineError('stale')
        })
    }
    let openPromise: Promise<void> | undefined
    const openStore = () => (openPromise ??= dependencies.store.open().finally(() => {
        openPromise = undefined
    }))
    const acquireCurrentRevisionReader = (): Promise<PersistentRevisionLease> =>
        acquireCurrentRevisionWithRetry(
            (revision) => dependencies.store.acquireRevision(revision),
            async () => (await dependencies.store.readRoot()).revision,
        )
    const prepareQuery = async (signal?: AbortSignal) => {
        throwIfQueryAborted(signal)
        await dependencies.flushPendingData('plugin-database-query')
        throwIfQueryAborted(signal)
        await openStore()
        throwIfQueryAborted(signal)
    }
    const rejectIdentityReplacement = (
        context: PluginFullObjectCallContext,
        operation: PluginIdentityReplacementOperation,
        targetId: string,
        attemptedId: string,
    ): never => {
        const diagnostic = {
            kind: 'plugin-identity-replacement-rejected',
            pluginName: context.pluginName,
            operation,
            targetId,
            attemptedId,
        } satisfies PluginIdentityReplacementDiagnostic
        dependencies.reportIdentityReplacementRejected(diagnostic)
        throw new PluginIdentityReplacementRejectedError(diagnostic)
    }
    const acquireSelectedLease = async (
        selectedTarget: SelectedConversationTarget | null,
        characterId: string,
        conversationId?: string,
    ): Promise<CompleteConversationLease | null> => {
        if (!selectedTarget || selectedTarget.characterId !== characterId) return null
        if (conversationId !== undefined && selectedTarget.conversationId !== conversationId) {
            return null
        }
        return dependencies.acquireCompleteConversation(
            'plugin-full-object-setter',
            selectedTarget,
        )
    }
    const captureSelectedCallBoundary = () => {
        const authorityEpoch = dependencies.getStorageAuthorityEpoch()
        dependencies.assertPersistentMutationAllowed(authorityEpoch)
        return {
            authorityEpoch,
            characterId: dependencies.getSelectedCharacterId(),
            navigationGeneration: dependencies.getNavigationGeneration(),
            target: dependencies.captureSelectedConversationTarget(),
        }
    }
    const recaptureSelectedTarget = (boundary: ReturnType<
        typeof captureSelectedCallBoundary
    >): SelectedConversationTarget | null => {
        dependencies.assertPersistentMutationAllowed(boundary.authorityEpoch)
        const target = dependencies.captureSelectedConversationTarget()
        if (
            !boundary.target ||
            !target ||
            boundary.characterId !== boundary.target.characterId ||
            boundary.navigationGeneration !== boundary.target.navigationGeneration ||
            dependencies.getSelectedCharacterId() !== boundary.characterId ||
            dependencies.getNavigationGeneration() !== boundary.navigationGeneration ||
            target.navigationGeneration !== boundary.navigationGeneration ||
            target.characterId !== boundary.target.characterId ||
            target.conversationId !== boundary.target.conversationId
        ) return null
        return target
    }
    // A leaseless scoped write is durably fenced by expectedRevision, but if
    // it lands on the currently selected conversation (stale call boundary
    // after navigating away and back), the published working-set replacement
    // detaches any live session from its conversation object. Invalidate the
    // session so it re-establishes against the replaced object.
    const invalidateLeaselessSelectedWrite = (
        characterId: string,
        conversationId?: string,
    ): void => {
        if (dependencies.getSelectedCharacterId() !== characterId) return
        if (conversationId !== undefined) {
            const current = dependencies.captureSelectedConversationTarget()
            if (!current || current.conversationId !== conversationId) return
        }
        dependencies.invalidateActiveConversationSession?.()
    }

    return {
        expireReadBaselines: () => readBaselines.expire(),
        closeReadBaselines: () => readBaselines.close(),
        async getFullObjectSnapshotStream(target, context) {
            const readAuthority = dependencies.getStorageAuthorityEpoch()
            throwIfFullObjectCallAborted(context.signal)
            await dependencies.flushPendingData('plugin-full-object-read')
            throwIfFullObjectCallAborted(context.signal)
            const selectedId = dependencies.getSelectedCharacterId()
            if (target.characterIndex === undefined && selectedId === null) return undefined
            await openStore()
            const reader = await acquireCurrentRevisionReader()
            let transferred = false
            try {
                throwIfFullObjectCallAborted(context.signal)
                const resolved = target.characterIndex === undefined
                    ? { characterId: selectedId! }
                    : target.chatIndex === undefined
                        ? await resolvePinnedCharacterTarget(reader, target.characterIndex)
                        : await resolvePinnedConversationTarget(reader, target.characterIndex, target.chatIndex)
                if (!resolved) return null
                const detail = await reader.readCharacter(resolved.characterId)
                if (!detail) return target.characterIndex === undefined ? undefined : null
                assertPinnedRevision(reader.revision, detail.revision, 'Character')
                const chunks = (async function* (): AsyncGenerator<PluginDatabaseSnapshotChunk> {
                    let baseline: Record<string, any> | undefined = { characters: [] }
                    yield { type: 'arrayStart', key: 'characters' }
                    if ('conversationId' in resolved) {
                        baseline.characters.push({ ...structuredClone(detail.value as object), chats: [] })
                        yield { type: 'characterStart', key: 'characters', value: detail.value }
                        for await (const chunk of streamPinnedConversation(reader, resolved.characterId, String(resolved.conversationId))) {
                            accumulatePluginSnapshotChunk(baseline, chunk)
                            yield chunk
                        }
                    } else {
                        for await (const chunk of streamPinnedCharacter(reader, resolved.characterId, detail.value)) {
                            accumulatePluginSnapshotChunk(baseline, chunk)
                            yield chunk
                        }
                    }
                    if ('conversationId' in resolved) readBaselines.track(baseline.characters[0].chats[0], 'conversation', JSON.stringify([resolved.characterId, resolved.conversationId]), reader.revision, readAuthority)
                    else readBaselines.track(baseline.characters[0], 'character', resolved.characterId, reader.revision, readAuthority)
                    const entries = collectPluginReadProvenance(baseline)
                    baseline = undefined
                    yield { type: 'provenance', entries }
                })()
                let closed = false
                const close = async () => {
                    if (closed) return
                    closed = true
                    context.signal.removeEventListener('abort', abort)
                    try { await chunks.return(undefined) }
                    finally { await releasePersistentRevisionLease(reader) }
                }
                const abort = () => { void close().catch(() => undefined) }
                context.signal.addEventListener('abort', abort, { once: true })
                const value = new ReadableStream<PluginDatabaseSnapshotChunk>({
                    async pull(controller) {
                        try {
                            throwIfFullObjectCallAborted(context.signal)
                            const next = await chunks.next()
                            throwIfFullObjectCallAborted(context.signal)
                            if (next.done) {
                                await close()
                                controller.close()
                            } else controller.enqueue(next.value)
                        } catch (error) {
                            await close().catch(() => undefined)
                            controller.error(error)
                        }
                    },
                    cancel: close,
                })
                transferred = true
                return {
                    __type: 'IFRAME_OBJECT_STREAM', value,
                    select: target.chatIndex === undefined ? 'character' : 'conversation',
                }
            } finally {
                if (!transferred) await releasePersistentRevisionLease(reader)
            }
        },

        async getCurrentCharacter(context) {
            const readAuthority = dependencies.getStorageAuthorityEpoch()
            throwIfFullObjectCallAborted(context.signal)
            await dependencies.flushPendingData('plugin-full-object-read')
            throwIfFullObjectCallAborted(context.signal)
            const characterId = dependencies.getSelectedCharacterId()
            if (characterId === null) {
                return undefined
            }
            await openStore()
            const lease = await acquireCurrentRevisionReader()
            return withPersistentRevisionLease(lease, async (reader) => {
                throwIfFullObjectCallAborted(context.signal)
                const value = await readPinnedCompleteCharacter(reader, characterId)
                throwIfFullObjectCallAborted(context.signal)
                return value === null ? undefined : readBaselines.track(dependencies.snapshot(value), 'character', characterId, reader.revision, readAuthority)
            })
        },

        async getCharacterFromIndex(index, context) {
            const readAuthority = dependencies.getStorageAuthorityEpoch()
            throwIfFullObjectCallAborted(context.signal)
            await dependencies.flushPendingData('plugin-full-object-read')
            throwIfFullObjectCallAborted(context.signal)
            await openStore()
            const lease = await acquireCurrentRevisionReader()
            return withPersistentRevisionLease(lease, async (reader) => {
                throwIfFullObjectCallAborted(context.signal)
                const target = await resolvePinnedCharacterTarget(reader, index)
                const value = target
                    ? await readPinnedCompleteCharacter(reader, target.characterId)
                    : null
                throwIfFullObjectCallAborted(context.signal)
                return value === null ? null : readBaselines.track(dependencies.snapshot(value), 'character', target!.characterId, reader.revision, readAuthority)
            })
        },

        async getChatFromIndex(characterIndex, chatIndex, context) {
            const readAuthority = dependencies.getStorageAuthorityEpoch()
            throwIfFullObjectCallAborted(context.signal)
            await dependencies.flushPendingData('plugin-full-object-read')
            throwIfFullObjectCallAborted(context.signal)
            await openStore()
            const lease = await acquireCurrentRevisionReader()
            return withPersistentRevisionLease(lease, async (reader) => {
                throwIfFullObjectCallAborted(context.signal)
                const target = await resolvePinnedConversationTarget(
                    reader,
                    characterIndex,
                    chatIndex,
                )
                const found = target
                    ? await reader.readConversation(target.characterId, target.conversationId)
                    : null
                if (found) {
                    assertPinnedRevision(
                        reader.revision,
                        found.revision,
                        `Conversation ${target!.conversationId}`,
                    )
                }
                throwIfFullObjectCallAborted(context.signal)
                return found === null ? null : readBaselines.track(dependencies.snapshot(found.value), 'conversation', JSON.stringify([target!.characterId, target!.conversationId]), reader.revision, readAuthority)
            })
        },

        async setCurrentCharacter(character, context) {
            validatePluginCompleteCharacter(character)
            const admission = readBaselines.hasRead('character', character.chaId) ? undefined : captureCharacterAdmission(dependencies.getSelectedCharacterId() ?? character.chaId)
            const candidate = snapshotWithProvenance(character)
            const selectedBoundary = captureSelectedCallBoundary()
            throwIfFullObjectCallAborted(context.signal)
            const intents = readBaselines.intent(candidate, 'character', candidate.chaId, admission ? await admission : {})
            await dependencies.flushPendingData('plugin-full-object-write')
            throwIfFullObjectCallAborted(context.signal)
            dependencies.assertPersistentMutationAllowed(selectedBoundary.authorityEpoch)
            const characterId = selectedBoundary.characterId
            if (characterId === null) return
            await openStore()
            const lease = await acquireCurrentRevisionReader()
            const target = await withPersistentRevisionLease(lease, async (reader) => {
                const found = await reader.readCharacter(characterId)
                if (!found) return null
                assertPinnedRevision(reader.revision, found.revision, `Character ${characterId}`)
                return { revision: reader.revision, characterId }
            })
            if (!target) throw new PluginFullObjectTargetStaleError(characterId)
            throwIfFullObjectCallAborted(context.signal)
            if (candidate.chaId !== target.characterId) {
                rejectIdentityReplacement(
                    context,
                    'setCharacter',
                    target.characterId,
                    candidate.chaId,
                )
            }
            let completeLease: CompleteConversationLease | null = null
            try {
                completeLease = await acquireSelectedLease(
                    recaptureSelectedTarget(selectedBoundary),
                    target.characterId,
                )
                throwIfFullObjectCallAborted(context.signal)
                dependencies.assertPersistentMutationAllowed(selectedBoundary.authorityEpoch)
                readBaselines.assertOpen()
                const changes = pluginUnitIntents(intents, 'character', dependencies.owner, candidate, target.characterId)
                await dependencies.commitPersistentUnitIntent('plugin-setCharacter', changes.units, [], changes.wholeMessages)
                if (!completeLease) invalidateLeaselessSelectedWrite(target.characterId)
            } finally {
                if (completeLease) {
                    completeLease.release()
                    dependencies.refreshSelectedConversationAfterReplacement(
                        completeLease.target,
                        completeLease.session,
                    )
                }
            }
        },

        async setCharacterToIndex(index, character, context) {
            validatePluginCompleteCharacter(character)
            const admission = readBaselines.hasRead('character', character.chaId) ? undefined : captureCharacterAdmission(character.chaId, index)
            const candidate = snapshotWithProvenance(character)
            const selectedBoundary = captureSelectedCallBoundary()
            throwIfFullObjectCallAborted(context.signal)
            const intents = readBaselines.intent(candidate, 'character', candidate.chaId, admission ? await admission : {})
            await dependencies.flushPendingData('plugin-full-object-write')
            throwIfFullObjectCallAborted(context.signal)
            dependencies.assertPersistentMutationAllowed(selectedBoundary.authorityEpoch)
            await openStore()
            const lease = await acquireCurrentRevisionReader()
            const target = await withPersistentRevisionLease(lease, async (reader) =>
                resolvePinnedCharacterTarget(reader, index))
            if (!target) return
            throwIfFullObjectCallAborted(context.signal)
            if (candidate.chaId !== target.characterId) {
                rejectIdentityReplacement(
                    context,
                    'setCharacterToIndex',
                    target.characterId,
                    candidate.chaId,
                )
            }
            let completeLease: CompleteConversationLease | null = null
            try {
                completeLease = await acquireSelectedLease(
                    recaptureSelectedTarget(selectedBoundary),
                    target.characterId,
                )
                throwIfFullObjectCallAborted(context.signal)
                dependencies.assertPersistentMutationAllowed(selectedBoundary.authorityEpoch)
                readBaselines.assertOpen()
                const changes = pluginUnitIntents(intents, 'character', dependencies.owner, candidate, target.characterId)
                await dependencies.commitPersistentUnitIntent('plugin-setCharacterToIndex', changes.units, [], changes.wholeMessages)
                if (!completeLease) invalidateLeaselessSelectedWrite(target.characterId)
            } finally {
                if (completeLease) {
                    completeLease.release()
                    dependencies.refreshSelectedConversationAfterReplacement(
                        completeLease.target,
                        completeLease.session,
                    )
                }
            }
        },

        async setChatToIndex(characterIndex, chatIndex, chat, context) {
            validatePluginCompleteChat(chat)
            const characterId = dependencies.getCompatibilityDatabase().characters?.[characterIndex]?.chaId
            const admission = characterId && readBaselines.hasRead('conversation', JSON.stringify([characterId, chat.id])) ? undefined : captureCharacterAdmission(characterId ?? '', characterIndex)
            const candidate = snapshotWithProvenance(chat)
            const selectedBoundary = captureSelectedCallBoundary()
            throwIfFullObjectCallAborted(context.signal)
            const admissionCharacter = admission ? await admission as PluginCompleteCharacter : undefined
            await dependencies.flushPendingData('plugin-full-object-write')
            throwIfFullObjectCallAborted(context.signal)
            dependencies.assertPersistentMutationAllowed(selectedBoundary.authorityEpoch)
            await openStore()
            const lease = await acquireCurrentRevisionReader()
            const target = await withPersistentRevisionLease(lease, async (reader) =>
                resolvePinnedConversationTarget(reader, characterIndex, chatIndex))
            if (!target) return
            throwIfFullObjectCallAborted(context.signal)
            if (candidate.id !== target.conversationId) {
                rejectIdentityReplacement(
                    context,
                    'setChatToIndex',
                    target.conversationId,
                    candidate.id!,
                )
            }
            const intents = readBaselines.intent(candidate, 'conversation', JSON.stringify([target.characterId, target.conversationId]), admissionCharacter?.chats?.find(value => value.id === candidate.id) ?? {})
            let completeLease: CompleteConversationLease | null = null
            try {
                completeLease = await acquireSelectedLease(
                    recaptureSelectedTarget(selectedBoundary),
                    target.characterId,
                    target.conversationId,
                )
                throwIfFullObjectCallAborted(context.signal)
                dependencies.assertPersistentMutationAllowed(selectedBoundary.authorityEpoch)
                readBaselines.assertOpen()
                const changes = pluginUnitIntents(intents, 'conversation', dependencies.owner, candidate, target.characterId, target.conversationId)
                await dependencies.commitPersistentUnitIntent('plugin-setChatToIndex', changes.units, [], changes.wholeMessages)
                if (!completeLease) {
                    invalidateLeaselessSelectedWrite(
                        target.characterId,
                        target.conversationId,
                    )
                }
            } finally {
                if (completeLease) {
                    completeLease.release()
                    dependencies.refreshSelectedConversationAfterReplacement(
                        completeLease.target,
                        completeLease.session,
                    )
                }
            }
        },

        async queryCharacters(input = {}) {
            const limit = positiveLimit(
                input.limit,
                PLUGIN_SUMMARY_QUERY_DEFAULT_LIMIT,
                PLUGIN_SUMMARY_QUERY_MAX_LIMIT,
            )
            await prepareQuery()
            return dependencies.store.queryCharacters({
                search: input.search,
                order: input.order ?? 'configured',
                trash: input.trash ?? false,
                limit,
                cursor: input.cursor,
            })
        },

        async queryConversations(input) {
            requiredId(input.characterId, 'characterId')
            const limit = positiveLimit(
                input.limit,
                PLUGIN_SUMMARY_QUERY_DEFAULT_LIMIT,
                PLUGIN_SUMMARY_QUERY_MAX_LIMIT,
            )
            await prepareQuery()
            return dependencies.store.queryConversations({
                characterId: input.characterId,
                order: input.order ?? 'configured',
                limit,
                cursor: input.cursor,
            })
        },

        async queryConversationMessages(input) {
            requiredId(input.characterId, 'characterId')
            requiredId(input.conversationId, 'conversationId')
            const query = {
                characterId: input.characterId,
                conversationId: input.conversationId,
                ...pluginMessageWindow(input),
            }

            await prepareQuery(input.signal)
            throwIfQueryAborted(input.signal)
            const result = await dependencies.store.readConversationWindow(query)
            throwIfQueryAborted(input.signal)
            return result ? { ...result.value, revision: result.revision } : null
        },

        async readConversationContext(request, options) {
            throwIfQueryAborted(options.signal)
            await dependencies.flushPendingData('plugin-conversation-context-read')
            throwIfQueryAborted(options.signal)
            await openStore()
            throwIfQueryAborted(options.signal)
            const lease = await acquireCurrentRevisionReader()
            return withPersistentRevisionLease(lease, async (reader) => {
                // The selected conversation is resolved after the pin, so it and every part agree.
                const selected = dependencies.captureSelectedConversationTarget()
                return readPinnedConversationContext(reader, request, {
                    selected: selected && {
                        characterId: selected.characterId,
                        conversationId: selected.conversationId,
                    },
                    allowPrivate: options.allowPrivate,
                    signal: options.signal,
                })
            })
        },

        async getDatabaseSnapshotStream(includeOnly, allowedKeys) {
            const readAuthority = dependencies.getStorageAuthorityEpoch()
            const requestedKeys =
                includeOnly === 'all'
                    ? [...allowedKeys]
                    : allowedKeys.filter((key) => includeOnly.includes(key))
            await dependencies.flushPendingData('plugin-full-database-snapshot')
            await openStore()
            const reader = await acquireCurrentRevisionReader()
            let releasePromise: Promise<void> | undefined
            const release = () => releasePromise ??= releasePersistentRevisionLease(reader)
            const iterator = (async function* (): AsyncGenerator<PluginDatabaseSnapshotChunk> {
                let failed = false
                try {
                    const pinnedRoot = await reader.readRoot()
                    assertPinnedRevision(reader.revision, pinnedRoot.revision, 'Root')
                    for (const key of requestedKeys) {
                        if (key === 'characters') {
                            yield { type: 'arrayStart', key }
                            for await (const character of iteratePinnedCharacters(reader)) {
                                yield* streamPinnedCharacter(reader, character.summary.id, character.detail)
                            }
                            continue
                        }
                        if (key === 'botPresets') {
                            yield { type: 'arrayStart', key }
                            const catalog = await reader.queryPresets()
                            assertPinnedRevision(reader.revision, catalog.revision, 'Preset catalog')
                            for (const summary of catalog.items) {
                                const preset = await reader.readPreset(summary.id)
                                if (!preset) throw new Error(`Missing preset ${summary.id}`)
                                assertPinnedRevision(
                                    reader.revision,
                                    preset.revision,
                                    `Preset ${summary.id}`,
                                )
                                yield { type: 'arrayPush', key, value: preset.value }
                            }
                            continue
                        }
                        if (key === 'pluginCustomStorage') {
                            yield { type: 'recordStart', key }
                            for await (const item of streamPinnedPluginValues(reader, dependencies.owner)) {
                                yield { type: 'recordSet', key, entryKey: item.key, value: item.value }
                            }
                            continue
                        }
                        yield {
                            type: 'set',
                            key,
                            value: (pinnedRoot.value as unknown as Record<string, unknown>)[key],
                        }
                    }
                } catch (error) {
                    failed = true
                    throw error
                } finally {
                    try {
                        await release()
                    } catch (error) {
                        if (!failed) throw error
                    }
                }
            })()
            // Released once tracked, so the stream holds no copy of the library after it ends.
            let baseline: Record<string, any> | undefined = {}
            return new ReadableStream<PluginDatabaseSnapshotChunk>({
                async pull(controller) {
                    try {
                        const next = await iterator.next()
                        if (next.done) {
                            if (baseline) {
                                readBaselines.track(baseline, 'database', 'database', reader.revision, readAuthority)
                                const entries = collectPluginReadProvenance(baseline)
                                baseline = undefined
                                controller.enqueue({ type: 'provenance', entries })
                            }
                            controller.close()
                        } else {
                            accumulatePluginSnapshotChunk(baseline!, next.value)
                            controller.enqueue(next.value)
                        }
                    } catch (error) {
                        controller.error(error)
                    }
                },
                async cancel() {
                    try {
                        await iterator.return(undefined)
                    } finally {
                        await release()
                    }
                },
            })
        },

        async getDatabaseSnapshot(includeOnly, allowedKeys) {
            const readAuthority = dependencies.getStorageAuthorityEpoch()
            const requestedKeys =
                includeOnly === 'all'
                    ? [...allowedKeys]
                    : allowedKeys.filter((key) => includeOnly.includes(key))
            const needsCharacters = requestedKeys.includes('characters')
            const needsPersistentPresets = requestedKeys.includes('botPresets')
            if (!needsCharacters && !needsPersistentPresets) {
                const compatibilityDatabase = dependencies.getCompatibilityDatabase()
                const result: Record<string, unknown> = {}
                for (const key of requestedKeys) {
                    if (key === 'pluginCustomStorage') {
                        await dependencies.flushPendingData('plugin-storage-snapshot')
                        result[key] = await dependencies.readPluginStorageSnapshot()
                    } else {
                        result[key] = dependencies.snapshot(
                            (compatibilityDatabase as unknown as Record<string, unknown>)[key],
                        )
                    }
                }
                return readBaselines.track(result, 'database', 'database', dependencies.getPersistentRevision(), readAuthority)
            }
            await dependencies.flushPendingData('plugin-full-database-snapshot')
            await openStore()
            const reader = await acquireCurrentRevisionReader()
            return withPersistentRevisionLease(reader, async (reader) => {
                const pinnedRoot = await reader.readRoot()
                assertPinnedRevision(reader.revision, pinnedRoot.revision, 'Root')
                const result: Record<string, unknown> = {}
                for (const key of requestedKeys) {
                    if (key === 'characters') {
                        const characters: Database['characters'] = []
                        for await (const character of iteratePinnedCharacters(reader)) {
                            const chats: Database['characters'][number]['chats'] = []
                            for await (const conversation of iteratePinnedConversations(
                                reader,
                                character.summary.id,
                            )) {
                                chats.push(conversation.value)
                            }
                            characters.push(dependencies.snapshot({
                                ...character.detail,
                                chats,
                            } as Database['characters'][number]))
                        }
                        result[key] = characters
                        continue
                    }
                    if (key === 'botPresets') {
                        const catalog = await reader.queryPresets()
                        assertPinnedRevision(
                            reader.revision,
                            catalog.revision,
                            'Preset catalog',
                        )
                        const presets: Database['botPresets'] = []
                        for (const summary of catalog.items) {
                            const preset = await reader.readPreset(summary.id)
                            if (!preset) throw new Error(`Missing preset ${summary.id}`)
                            assertPinnedRevision(
                                reader.revision,
                                preset.revision,
                                `Preset ${summary.id}`,
                            )
                            presets.push(dependencies.snapshot(preset.value))
                        }
                        result[key] = presets
                        continue
                    }
                    if (key === 'pluginCustomStorage') {
                        const storage: Record<string, unknown> = {}
                        for await (const item of streamPinnedPluginValues(reader, dependencies.owner)) {
                            defineOwnEnumerableProperty(storage, item.key, dependencies.snapshot(item.value))
                        }
                        result[key] = storage
                        continue
                    }
                    result[key] = dependencies.snapshot(
                        (pinnedRoot.value as unknown as Record<string, unknown>)[key],
                    )
                }
                return readBaselines.track(result, 'database', 'database', reader.revision, readAuthority)
            })
        },

        setDatabaseLite(database, allowedKeys) {
            const authority = dependencies.getStorageAuthorityEpoch()
            dependencies.assertPersistentMutationAllowed(authority)
            validatePluginDatabaseUpdate(database)
            if (hasCharacterUpdate(database)) throw new Error(SYNCHRONOUS_CHARACTER_SET_ERROR)
            const submitted = snapshotWithProvenance(database)
            const commit = (baseline: unknown) => {
                dependencies.assertPersistentMutationAllowed(authority)
                const intents = readBaselines.intent(submitted, 'database', 'database', baseline)
                const changes = databaseUnitChanges(intents.filter(intent => intent.path[0] !== 'plugins'), submitted, allowedKeys)
                return dependencies.commitPersistentUnitIntent('plugin-setDatabaseLite', changes.units, [], changes.wholeMessages)
            }
            if (Object.hasOwn(submitted, 'pluginCustomStorage') && !readBaselines.hasRead('database', 'database')) return captureDatabaseAdmission(submitted).then(commit)
            return commit(admissionDatabase())
        },

        async setDatabase(database, allowedKeys) {
            const authorityEpoch = dependencies.getStorageAuthorityEpoch()
            dependencies.assertPersistentMutationAllowed(authorityEpoch)
            validatePluginDatabaseUpdate(database)
            if (hasCharacterUpdate(database)) validateCompleteCharacters(database.characters)
            allowedKeys = [...allowedKeys]
            const initialNavigationGeneration = dependencies.getNavigationGeneration()
            const detachedSubmission = snapshotWithProvenance(database)
            const admission = readBaselines.hasRead('database', 'database') ? undefined : captureDatabaseAdmission(detachedSubmission)
            const intents = readBaselines.intent(detachedSubmission, 'database', 'database', admission ? await admission : {})
            readBaselines.assertOpen()
            if (hasCharacterUpdate(detachedSubmission)) {
                validateCompleteCharacters(detachedSubmission.characters)
            }
            const changedKeys = new Set(intents.map(intent => String(intent.path[0])))
            const detachedUpdate = Object.fromEntries(Object.entries(detachedSubmission).filter(([key]) => changedKeys.has(key)))
            const preparedUpdate = dependencies.prepareAuthoritativeDatabaseUpdate
                ? await dependencies.prepareAuthoritativeDatabaseUpdate(detachedUpdate)
                : detachedUpdate
            dependencies.assertPersistentMutationAllowed(authorityEpoch)
            readBaselines.assertOpen()
            validatePluginDatabaseUpdate(preparedUpdate)
            if (dependencies.getNavigationGeneration() !== initialNavigationGeneration) {
                throw new Error(STALE_DATABASE_SET_ERROR)
            }
            if (hasCharacterUpdate(preparedUpdate)) {
                validateCompleteCharacters(preparedUpdate.characters)
            }
            let acceptedIntents = intents
            if (Object.hasOwn(detachedUpdate, 'plugins')) {
                const installed = dependencies.getCompatibilityDatabase().plugins ?? []
                const installedNames = new Set(installed.map(plugin => plugin.name))
                const approved = new Map((preparedUpdate.plugins as RisuPlugin[] | undefined ?? []).map(plugin => [plugin.name, plugin]))
                const additions = intents.filter(intent => intent.path[0] === 'plugins' && intent.path.length === 2 && intent.path[1] !== '$order' && intent.type === 'set' && !installedNames.has(String(intent.path[1])) && approved.has(String(intent.path[1])))
                    .map(intent => ({ ...intent, value: approved.get(String(intent.path[1]))! }))
                // Whole-list API submissions request installation, while addressed storage deletes remain independent.
                acceptedIntents = [...intents.filter(intent => intent.path[0] !== 'plugins'), ...additions]
                if (additions.length) {
                    const addedNames = new Set(additions.map(intent => String(intent.path[1])))
                    acceptedIntents.push({ path: ['plugins', '$order'], type: 'set', value: [...installed.map(plugin => plugin.name), ...[...approved.keys()].filter(name => addedNames.has(name))] })
                }
            }
            const changes = databaseUnitChanges(acceptedIntents, preparedUpdate, allowedKeys)
            await dependencies.commitPersistentUnitIntent('plugin-setDatabase', changes.units, [], changes.wholeMessages)
            dependencies.assertPersistentMutationAllowed(authorityEpoch)

        },
    }
}

export function createProductionPluginDatabaseAccess(
    dependencies: Omit<PluginDatabaseAccessDependencies, 'store'>,
): PluginDatabaseAccess {
    return createPluginDatabaseAccess({
        ...dependencies,
        store: getPersistentDataStore(),
    })
}

export function createProductionPluginChatOutputProjector(
    snapshot: <T>(value: T) => T,
): PluginChatOutputProjector {
    let store: ReturnType<typeof getPersistentDataStore> | undefined
    let openPromise: Promise<void> | undefined
    return async (input) => {
        const persistentStore = store ??= getPersistentDataStore()
        await (openPromise ??= persistentStore.open().finally(() => {
            openPromise = undefined
        }))
        const lease = await acquireCurrentRevisionWithRetry(
            (revision) => persistentStore.acquireRevision(revision),
            async () => (await persistentStore.readRoot()).revision,
        )
        return withPersistentRevisionLease(lease, async (reader) => {
            const detail = await reader.readCharacter(input.characterId)
            if (!detail) throw new Error(`Missing listener character ${input.characterId}`)
            assertPinnedRevision(reader.revision, detail.revision, 'Listener character')

            const liveCompleteChats = new Map(
                input.liveCharacter.chats
                    .filter((chat) => !isConversationSummaryStub(chat) && chat.id)
                    .map((chat) => [chat.id!, snapshot(chat)]),
            )
            liveCompleteChats.set(input.conversationId, snapshot(input.liveConversation))

            const chats: Chat[] = []
            let cursor: string | undefined
            do {
                const page = await reader.queryConversations({ characterId: input.characterId, order: 'configured', limit: 100, cursor })
                assertPinnedRevision(reader.revision, page.revision, 'Listener conversations')
                for (const summary of page.items) {
                    const live = liveCompleteChats.get(summary.id)
                    if (live) chats.push(live)
                    else {
                        const conversation = await reader.readConversation(input.characterId, summary.id)
                        if (!conversation) throw new Error(`Missing listener conversation ${summary.id}`)
                        assertPinnedRevision(reader.revision, conversation.revision, 'Listener conversation')
                        chats.push(conversation.value as Chat)
                    }
                }
                cursor = page.nextCursor
            } while (cursor !== undefined)
            const { chats: _liveChats, ...liveDetail } = snapshot(input.liveCharacter)
            const char = {
                ...detail.value,
                ...liveDetail,
                chats,
            } as PluginCompleteCharacter
            const chat = char.chats.find(
                (candidate) => candidate.id === input.conversationId,
            )
            if (!chat) throw new Error(`Missing listener conversation ${input.conversationId}`)
            return { char: snapshot(char), chat: snapshot(chat) }
        })
    }
}
