import type { Message } from '../storage/database.svelte'
import type {
    ConversationMutation,
    DataRevision,
    PersistentRevisionReader,
    PersistentUnitMutation,
} from '../storage/persistentDataStore'
import { assertPinnedRevision } from '../storage/persistentRecordIterator'
import { responseEditReplacement } from '../responseVariants'
import { isPlainObject, isPluginMessageField, requiredId } from './pluginQueryInput'

export const CONVERSATION_PATCH_MAX_MESSAGES = 64
export const CONVERSATION_PATCH_MAX_CHAT_VARIABLES = 64
export const CONVERSATION_PATCH_MAX_MUTATION_ID_LENGTH = 128
export const CONVERSATION_PATCH_MAX_FIELD_BYTES = 1_048_576
export const CONVERSATION_PATCH_LEDGER_LIMIT = 128
export const CONVERSATION_PATCH_LEDGER_RETENTION_MS = 5 * 60_000

const SPAN_READ_LIMIT = 16

export type ChatVariableValue = string | number | boolean

export interface ConversationPatchMessage {
    index: number
    messageId: string | null
    expected?: Record<string, unknown>
    set: Record<string, unknown>
}

export interface ConversationPatchChatVariable {
    key: string
    /** Absent when the plugin does not check the current value; null requires the key to be absent. */
    expected?: ChatVariableValue | null
    value: ChatVariableValue | null
}

export interface ConversationPatchRequest {
    characterId: string
    conversationId: string
    mutationId: string
    baseRevision?: DataRevision
    messages: ConversationPatchMessage[]
    chatVariables: ConversationPatchChatVariable[]
}

export interface ConversationPatchConflict {
    target: 'conversation' | 'message' | 'chatVariable'
    reason: 'not-found' | 'mismatch' | 'revision'
    index?: number
    field?: string
    key?: string
}

export interface ConversationPatchResult {
    status: 'applied' | 'already-applied' | 'conflict' | 'busy'
    conflict?: ConversationPatchConflict
    revision: DataRevision
}

export interface ConversationPatchRun {
    start: number
    messages: Message[]
}

export interface PreparedConversationPatch {
    unitMutations: PersistentUnitMutation[]
    conversations: ConversationMutation[]
}

export type ConversationPatchPlan =
    | { kind: 'conflict'; conflict: ConversationPatchConflict }
    | { kind: 'apply'; runs: ConversationPatchRun[]; scriptstate: Record<string, unknown> | null }

function safeIndex(value: unknown, name: string): number {
    if (!Number.isSafeInteger(value) || (value as number) < 0) {
        throw new RangeError(`${name} must be a nonnegative safe integer`)
    }
    return value as number
}

function assertJsonValue(value: unknown, path: string, ancestors: unknown[] = []): void {
    if (value === null || typeof value === 'string' || typeof value === 'boolean') return
    if (typeof value === 'number') {
        if (!Number.isFinite(value)) throw new TypeError(`${path} must be a finite number`)
        return
    }
    if (typeof value !== 'object') throw new TypeError(`${path} is not JSON-serializable`)
    if (ancestors.includes(value)) throw new TypeError(`${path} contains a cycle`)
    const nested = [...ancestors, value]
    if (Array.isArray(value)) {
        for (let index = 0; index < value.length; index++) {
            if (!(index in value)) throw new TypeError(`${path}[${index}] is missing`)
            assertJsonValue(value[index], `${path}[${index}]`, nested)
        }
        return
    }
    if (!isPlainObject(value) || typeof (value as { toJSON?: unknown }).toJSON === 'function') {
        throw new TypeError(`${path} is not JSON-serializable`)
    }
    for (const [key, item] of Object.entries(value)) assertJsonValue(item, `${path}.${key}`, nested)
}

function serializedBytes(value: unknown): number {
    return new TextEncoder().encode(JSON.stringify(value)).byteLength
}

function cloneJson<T>(value: T): T {
    return JSON.parse(JSON.stringify(value)) as T
}

function chatVariableValue(value: unknown, path: string, allowNull: boolean): ChatVariableValue | null {
    if (value === null && allowNull) return null
    if (typeof value === 'string' || typeof value === 'boolean') return value
    if (typeof value === 'number' && Number.isFinite(value)) return value
    throw new TypeError(`${path} must be a string, a finite number, a boolean or null`)
}

function normalizeMessage(input: unknown, position: number, baseRevision: number | undefined): ConversationPatchMessage {
    const path = `messages[${position}]`
    if (!isPlainObject(input)) throw new TypeError(`${path} must be an object`)
    const index = safeIndex(input.index, `${path}.index`)
    if (input.messageId !== null && typeof input.messageId !== 'string') {
        throw new TypeError(`${path}.messageId must be a string or null`)
    }
    const messageId = input.messageId as string | null
    if (messageId === null && baseRevision === undefined) {
        throw new TypeError(`${path} addresses a message without an ID, so baseRevision is required`)
    }
    let expected: Record<string, unknown> | undefined
    if (input.expected !== undefined) {
        if (!isPlainObject(input.expected)) throw new TypeError(`${path}.expected must be an object`)
        expected = {}
        for (const [field, value] of Object.entries(input.expected)) {
            if (value !== undefined) assertJsonValue(value, `${path}.expected.${field}`)
            expected[field] = value === undefined ? undefined : cloneJson(value)
        }
    }
    if (!isPlainObject(input.set)) throw new TypeError(`${path}.set must be an object`)
    const set: Record<string, unknown> = {}
    for (const [field, value] of Object.entries(input.set)) {
        if (field === 'data') {
            if (typeof value !== 'string') throw new TypeError(`${path}.set.data must be a string`)
        } else if (!isPluginMessageField(field)) {
            throw new TypeError(`${path}.set.${field} is not data or a plugin message field`)
        } else if (value !== undefined) {
            assertJsonValue(value, `${path}.set.${field}`)
            if (serializedBytes(value) > CONVERSATION_PATCH_MAX_FIELD_BYTES) {
                throw new RangeError(`${path}.set.${field} exceeds ${CONVERSATION_PATCH_MAX_FIELD_BYTES} bytes`)
            }
        }
        set[field] = value === undefined ? undefined : cloneJson(value)
    }
    return expected === undefined ? { index, messageId, set } : { index, messageId, expected, set }
}

function normalizeChatVariable(input: unknown, position: number): ConversationPatchChatVariable {
    const path = `chatVariables[${position}]`
    if (!isPlainObject(input)) throw new TypeError(`${path} must be an object`)
    requiredId(input.key as string, `${path}.key`)
    const entry: ConversationPatchChatVariable = {
        key: input.key as string,
        value: chatVariableValue(input.value, `${path}.value`, true),
    }
    if (input.expected !== undefined) entry.expected = chatVariableValue(input.expected, `${path}.expected`, true)
    return entry
}

/** Validates plugin input before any read or commit; invalid input rejects the call. */
export function normalizeConversationPatchInput(input: unknown): ConversationPatchRequest {
    if (!isPlainObject(input)) throw new TypeError('Conversation patch input must be an object')
    requiredId(input.characterId as string, 'characterId')
    requiredId(input.conversationId as string, 'conversationId')
    const mutationId = input.mutationId
    if (typeof mutationId !== 'string' || mutationId.length === 0 ||
        mutationId.length > CONVERSATION_PATCH_MAX_MUTATION_ID_LENGTH) {
        throw new RangeError(`mutationId must be a string of 1 to ${CONVERSATION_PATCH_MAX_MUTATION_ID_LENGTH} characters`)
    }
    const baseRevision = input.baseRevision === undefined ? undefined : safeIndex(input.baseRevision, 'baseRevision')
    const messages: ConversationPatchMessage[] = []
    if (input.messages !== undefined) {
        if (!Array.isArray(input.messages)) throw new TypeError('messages must be an array')
        if (input.messages.length > CONVERSATION_PATCH_MAX_MESSAGES) {
            throw new RangeError(`messages accepts at most ${CONVERSATION_PATCH_MAX_MESSAGES} entries`)
        }
        const indices = new Set<number>()
        input.messages.forEach((entry, position) => {
            const message = normalizeMessage(entry, position, baseRevision)
            if (indices.has(message.index)) throw new RangeError(`messages repeats index ${message.index}`)
            indices.add(message.index)
            messages.push(message)
        })
    }
    const chatVariables: ConversationPatchChatVariable[] = []
    if (input.chatVariables !== undefined) {
        if (!Array.isArray(input.chatVariables)) throw new TypeError('chatVariables must be an array')
        if (input.chatVariables.length > CONVERSATION_PATCH_MAX_CHAT_VARIABLES) {
            throw new RangeError(`chatVariables accepts at most ${CONVERSATION_PATCH_MAX_CHAT_VARIABLES} entries`)
        }
        const keys = new Set<string>()
        input.chatVariables.forEach((entry, position) => {
            const variable = normalizeChatVariable(entry, position)
            if (keys.has(variable.key)) throw new RangeError(`chatVariables repeats key ${variable.key}`)
            keys.add(variable.key)
            chatVariables.push(variable)
        })
    }
    const request: ConversationPatchRequest = {
        characterId: input.characterId as string,
        conversationId: input.conversationId as string,
        mutationId,
        messages,
        chatVariables,
    }
    if (baseRevision !== undefined) request.baseRevision = baseRevision
    return request
}

export function patchSetsMessageData(request: ConversationPatchRequest): boolean {
    return request.messages.some((entry) => Object.hasOwn(entry.set, 'data'))
}

function jsonEqual(left: unknown, right: unknown): boolean {
    if (left === right) return true
    if (left === null || right === null || typeof left !== 'object' || typeof right !== 'object') return false
    if (Array.isArray(left) !== Array.isArray(right)) return false
    if (Array.isArray(left)) {
        const other = right as unknown[]
        return left.length === other.length && left.every((item, index) => jsonEqual(item, other[index]))
    }
    const leftEntries = Object.entries(left).filter(([, value]) => value !== undefined)
    const rightRecord = right as Record<string, unknown>
    const rightCount = Object.values(rightRecord).filter((value) => value !== undefined).length
    return leftEntries.length === rightCount &&
        leftEntries.every(([key, value]) => Object.hasOwn(rightRecord, key) && jsonEqual(value, rightRecord[key]))
}

function isTurnStart(message: Message): boolean {
    return message.role === 'user' && !message.isComment
}

interface ConversationPatchSource {
    totalMessages: number
    message(index: number): Message
    /** Whether the message at `index` is the only one with this ID; read only without a base revision. */
    isUniqueMessageId(messageId: string, index: number): boolean
    scriptstate: Record<string, unknown> | undefined
}

function findConflict(
    request: ConversationPatchRequest,
    source: ConversationPatchSource,
    revisionMatches: boolean,
): ConversationPatchConflict | null {
    if (request.baseRevision !== undefined && !revisionMatches) {
        return { target: 'conversation', reason: 'revision' }
    }
    for (const entry of request.messages) {
        if (entry.index >= source.totalMessages) {
            return { target: 'message', reason: 'not-found', index: entry.index }
        }
        const message = source.message(entry.index)
        if ((message.chatId ?? null) !== entry.messageId) {
            return { target: 'message', reason: 'mismatch', index: entry.index, field: 'chatId' }
        }
        if (request.baseRevision === undefined && !source.isUniqueMessageId(entry.messageId!, entry.index)) {
            throw new TypeError(`Message ${entry.index} shares its chatId with another message, so baseRevision is required`)
        }
        for (const [field, value] of Object.entries(entry.expected ?? {})) {
            const current = (message as unknown as Record<string, unknown>)[field]
            if (value === undefined ? current !== undefined : !jsonEqual(current, value)) {
                return { target: 'message', reason: 'mismatch', index: entry.index, field }
            }
        }
    }
    for (const variable of request.chatVariables) {
        if (!Object.hasOwn(variable, 'expected')) continue
        const current = source.scriptstate?.[variable.key]
        if (variable.expected === null ? current !== undefined : current !== variable.expected) {
            return { target: 'chatVariable', reason: 'mismatch', key: variable.key }
        }
    }
    return null
}

function applySet(message: Message, set: Record<string, unknown>): Message {
    const updated = { ...message } as Record<string, unknown>
    for (const [field, value] of Object.entries(set)) {
        if (value === undefined) delete updated[field]
        else updated[field] = cloneJson(value)
    }
    return updated as unknown as Message
}

/** Each change takes the user edit's replacement, so a selected response variant keeps its snapshot in step. */
function planRuns(request: ConversationPatchRequest, source: ConversationPatchSource): ConversationPatchRun[] {
    const working = new Map<number, Message>()
    const current = (index: number) => working.get(index) ?? source.message(index)
    for (const entry of [...request.messages].sort((left, right) => left.index - right.index)) {
        let end = entry.index + 1
        while (end < source.totalMessages && !isTurnStart(current(end))) end++
        const span: Message[] = []
        for (let index = entry.index; index < end; index++) span.push(current(index))
        const replacement = responseEditReplacement(span, 0, applySet(span[0], entry.set))
        replacement.forEach((message, offset) => working.set(entry.index + offset, message))
    }
    const changed = [...working]
        .filter(([index, message]) => JSON.stringify(message) !== JSON.stringify(source.message(index)))
        .sort(([left], [right]) => left - right)
    const runs: ConversationPatchRun[] = []
    for (const [index, message] of changed) {
        const last = runs.at(-1)
        if (last && last.start + last.messages.length === index) last.messages.push(message)
        else runs.push({ start: index, messages: [message] })
    }
    return runs
}

function planScriptstate(request: ConversationPatchRequest, current: Record<string, unknown> | undefined): Record<string, unknown> | null {
    if (!request.chatVariables.length) return null
    const next: Record<string, unknown> = { ...current }
    for (const variable of request.chatVariables) {
        if (variable.value === null) delete next[variable.key]
        else next[variable.key] = variable.value
    }
    return jsonEqual(next, current ?? {}) ? null : next
}

function plan(request: ConversationPatchRequest, source: ConversationPatchSource, revisionMatches: boolean): ConversationPatchPlan {
    const conflict = findConflict(request, source, revisionMatches)
    if (conflict) return { kind: 'conflict', conflict }
    return { kind: 'apply', runs: planRuns(request, source), scriptstate: planScriptstate(request, source.scriptstate) }
}

/** Checks and plans a patch against messages held in memory, such as the active session's. */
export function planLiveConversationPatch(
    request: ConversationPatchRequest,
    messages: readonly Message[],
    scriptstate: Record<string, unknown> | undefined,
    revisionMatches: boolean,
): ConversationPatchPlan {
    let occurrences: Map<string, number> | undefined
    return plan(request, {
        totalMessages: messages.length,
        message: (index) => messages[index],
        isUniqueMessageId(messageId) {
            if (!occurrences) {
                const addressed = new Set(request.messages.map((entry) => entry.messageId))
                occurrences = new Map()
                for (const message of messages) {
                    if (message.chatId != null && addressed.has(message.chatId)) {
                        occurrences.set(message.chatId, (occurrences.get(message.chatId) ?? 0) + 1)
                    }
                }
            }
            return occurrences.get(messageId) === 1
        },
        scriptstate,
    }, revisionMatches)
}

/** Checks and plans a patch at the reader's pinned revision, reading only the addressed responses. */
export async function planStoredConversationPatch(
    reader: PersistentRevisionReader,
    request: ConversationPatchRequest,
): Promise<ConversationPatchPlan> {
    const { characterId, conversationId } = request
    const metadata = await reader.readConversationMetadata(characterId, conversationId)
    if (!metadata) return { kind: 'conflict', conflict: { target: 'conversation', reason: 'not-found' } }
    assertPinnedRevision(reader.revision, metadata.revision, 'Patch conversation')
    const totalMessages = metadata.value.totalMessages
    const loaded = new Map<number, Message>()
    const load = async (startIndex: number) => {
        const window = await reader.readConversationWindow({ characterId, conversationId, startIndex, limit: SPAN_READ_LIMIT })
        if (!window) throw new Error('Patch conversation disappeared from its pinned revision')
        assertPinnedRevision(reader.revision, window.revision, 'Patch messages')
        window.value.messages.forEach((message, offset) => loaded.set(window.value.startIndex + offset, message))
    }
    for (const entry of request.messages) {
        for (let index = entry.index; index < totalMessages; index++) {
            if (!loaded.has(index)) await load(index)
            if (index > entry.index && isTurnStart(loaded.get(index)!)) break
        }
    }
    const unique = new Map<number, boolean>()
    if (request.baseRevision === undefined) {
        for (const entry of request.messages) {
            const message = loaded.get(entry.index)
            if (!message || message.chatId !== entry.messageId) continue
            const occurrence = async (anchorOccurrence: 'first' | 'last') => {
                const window = await reader.readConversationWindow({
                    characterId, conversationId, anchorMessageId: entry.messageId!, anchorOccurrence, before: 0, after: 0,
                })
                if (window) assertPinnedRevision(reader.revision, window.revision, 'Patch message occurrence')
                return window?.value.startIndex
            }
            unique.set(entry.index, await occurrence('first') === entry.index && await occurrence('last') === entry.index)
        }
    }
    return plan(request, {
        totalMessages,
        message: (index) => loaded.get(index)!,
        isUniqueMessageId: (_messageId, index) => unique.get(index) === true,
        scriptstate: metadata.value.conversation.scriptstate as Record<string, unknown> | undefined,
    }, reader.revision === request.baseRevision)
}

/** The store writes for a planned patch: one count-preserving range per run and the `scriptstate` unit. */
export function prepareConversationPatchCommit(
    request: ConversationPatchRequest,
    planned: Extract<ConversationPatchPlan, { kind: 'apply' }>,
): PreparedConversationPatch {
    const { characterId, conversationId } = request
    return {
        unitMutations: planned.scriptstate === null ? [] : [{
            key: JSON.stringify(['conversation', characterId, conversationId, 'scriptstate']),
            type: 'set',
            value: planned.scriptstate,
        }],
        conversations: planned.runs.map((run) => ({
            type: 'replace-range',
            characterId,
            conversationId,
            start: run.start,
            deleteCount: run.messages.length,
            messages: run.messages,
        })),
    }
}

/** Patch outcomes kept per plugin, so a retried mutation ID returns its first outcome. */
export function createConversationPatchLedger(now: () => number = Date.now) {
    const entries = new Map<string, { at: number; outcome: Promise<ConversationPatchResult> }>()
    return {
        run(mutationId: string, execute: () => Promise<ConversationPatchResult>): Promise<ConversationPatchResult> {
            const time = now()
            for (const [id, entry] of entries) {
                if (time - entry.at <= CONVERSATION_PATCH_LEDGER_RETENTION_MS) break
                entries.delete(id)
            }
            const existing = entries.get(mutationId)
            if (existing) {
                return existing.outcome.then((result) =>
                    result.status === 'applied' ? { ...result, status: 'already-applied' } : result)
            }
            const outcome = execute()
            const entry = { at: time, outcome }
            entries.set(mutationId, entry)
            while (entries.size > CONVERSATION_PATCH_LEDGER_LIMIT) entries.delete(entries.keys().next().value!)
            const forget = () => {
                if (entries.get(mutationId) === entry) entries.delete(mutationId)
            }
            outcome.then((result) => {
                if (result.status === 'busy') forget()
            }, forget)
            return outcome
        },
    }
}
