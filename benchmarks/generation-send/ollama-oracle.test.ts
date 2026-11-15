import { afterEach, expect, it, vi } from 'vitest'
import 'fake-indexeddb/auto'
import { readFileSync, writeFileSync } from 'node:fs'
import { resolve } from 'node:path'
import { createHash } from 'node:crypto'
import { setImmediate as immediate } from 'node:timers/promises'

const startup = vi.hoisted(() => ({ roots: [] as (() => void)[], ready: false, dispose: [] as (() => void)[] }))
vi.mock('svelte/internal/client', async (original) => {
    const actual = await original<typeof import('svelte/internal/client')>()
    return { ...actual, effect_root: (fn: () => void) => {
        if (startup.ready) return actual.effect_root(fn)
        let dispose: (() => void) | undefined
        startup.roots.push(() => { dispose = actual.effect_root(fn); startup.dispose.push(dispose) })
        return () => dispose?.()
    } }
})

const loreSetter = vi.hoisted(() => ({ assignments: null as { name: string; value: string }[] | null }))
vi.mock('../../src/ts/process/selectedChatVariables', async (original) => {
    const actual = await original<typeof import('../../src/ts/process/selectedChatVariables')>()
    return { ...actual, setSelectedChatVariable: (...args: Parameters<typeof actual.setSelectedChatVariable>) => {
        const result = actual.setSelectedChatVariable(...args)
        loreSetter.assignments?.push({ name: args[0], value: args[1] })
        return result
    } }
})

vi.mock('../../src/ts/process/luaRuntime', async (original) => (await import('./happyDomLuaRuntime')).luaRuntimeWithNodeLoader(original))

import { deriveEffectivePresetMirrors, normalizeDatabaseDefaults, setDatabase, getDatabase, type Database } from '../../src/ts/storage/database.svelte'
import { deriveEffectivePersonaMirrors } from '../../src/ts/storage/effectiveIdentityState'
import { selectedCharID } from '../../src/ts/stores.svelte'
import { characterFormatUpdate } from '../../src/ts/characters'
import { getPersistentDataStore } from '../../src/ts/storage/persistentDataStoreFactory'
import { acquireCompleteConversation, captureSelectedConversationTarget, getPersistentDataRuntime, peekActiveConversationSession } from '../../src/ts/storage/persistentDataRuntime.svelte'
import { projectCompleteScalableWorkingSet } from '../../src/ts/storage/workingSetCatalog'
import { pluginV2 } from '../../src/ts/plugins/plugins.svelte'
import { chatProcessStage, sendChat } from '../../src/ts/process/index.svelte'
import { reserveGeneration } from '../../src/ts/process/generationState'
import { updateStartupExclusions } from '../../src/ts/storage/deviceSettings'
import { resetScriptCache } from '../../src/ts/process/scripts'
import { setConversationChatVariable } from '../../src/ts/process/selectedChatVariables'
import { prepareNativePersistenceValue } from '../../src/ts/storage/nativePersistenceValue'
import { luaJsonModuleResponse } from './happyDomLuaRuntime'
import type { RecoveryExclusion } from '../../src/ts/storage/startupExclusions'

type RecordValue = Record<string, unknown>
// `triggerEditsMessages` marks a start trigger that edits, cuts or appends stored messages, so the stored history is
// recorded instead of compared with the history the send read.
type OracleCase = { id: string; responseContent?: string; expectedOutput?: string; root?: RecordValue; preset?: RecordValue; character?: RecordValue; conversation?: RecordValue; messages?: unknown[]; expectedIdAssignments?: number[]; now?: number; random?: number[]; timeZone?: string; startupExclusions?: RecoveryExclusion[]; observeLore?: boolean; observeTriggers?: boolean; triggerEditsMessages?: boolean }
// `status` is the provider failure's HTTP status (500 when absent); `network-failure` is a fetch that rejects; `stop` is a
// start trigger that stops the send before any request.
type EffectCase = OracleCase & { outcome: 'success' | 'context-failure' | 'provider-failure' | 'network-failure' | 'abort' | 'abort-before-request' | 'invalid-host' | 'stop'; status?: number }
type LoreObservation = { assignments: { name: string; value: string }[]; requestScriptstate: RecordValue | null; storedScriptstate: RecordValue }
// `jsonUnsafe` lists the values the JSON output cannot hold (undefined, NaN, infinities, -0) by path, as `jsValue` writes them.
type TriggerObservation = { requestScriptstate: RecordValue | null; storedScriptstate: unknown; storedNote: unknown; storedMessages: unknown[];
    jsonUnsafe?: { at: string; value: string }[] }

type Fixtures = { root: RecordValue; preset: RecordValue; character: RecordValue; conversation: RecordValue; messages: unknown[]; cases: OracleCase[]; effectMessages: unknown[]; effects: EffectCase[] }
const fixtureSource = readFileSync(resolve('tests/fixtures/native-generation/ollama-text.json'), 'utf8')
const fixtures = JSON.parse(fixtureSource) as Fixtures

function databaseFor(entry: OracleCase): Database {
    const conversation = { ...fixtures.conversation, ...entry.conversation, message: entry.messages ?? fixtures.messages }
    const character = { ...fixtures.character, ...entry.character, chats: [conversation] }
    return structuredClone({ ...fixtures.root, ...entry.root, botPresets: [{ ...fixtures.preset, ...entry.preset }], characters: [character] }) as unknown as Database
}

// A native route reads only stored records, so TS is observed after loading them: the store reads the stored
// selections back, and the loaded database is installed again.
async function reload(label: string) {
    const store = getPersistentDataStore()
    const database = await store.materializeDatabase()
    const { revision } = await store.replaceFromDatabase(database)
    setDatabase(projectCompleteScalableWorkingSet(database, 'character', revision, new Set(['character'])))
    const runtime = getPersistentDataRuntime()
    await runtime.initializeActiveWorkingSet(getDatabase())
    await runtime.flushPendingDataLocally(label)
    await immediate()
}

// The output row's metadata, with the per-run identity and timing values reduced to their contracts.
function outputObservation(value: unknown, from: number, to: number) {
    const row = value as RecordValue
    const info = row.generationInfo as RecordValue | undefined
    const timing = (info?.stageTiming ?? {}) as Record<string, unknown>
    return {
        text: row.data, keys: Object.keys(row), role: row.role, saying: row.saying ?? null,
        timeWithinSend: typeof row.time === 'number' && row.time >= from && row.time <= to,
        chatIdIsGenerationId: typeof row.chatId === 'string' && row.chatId.length > 0 && row.chatId === info?.generationId,
        promptInfo: row.promptInfo ?? null,
        generationInfo: info ? {
            keys: Object.keys(info), model: info.model,
            inputTokens: info.inputTokens, outputTokens: info.outputTokens, maxContext: info.maxContext,
            stageTimingKeys: Object.keys(timing),
            stageTimingWholeMilliseconds: Object.values(timing).every(stage => Number.isInteger(stage) && (stage as number) >= 0),
            stage2: timing.stage2 ?? null,
        } : null,
    }
}

const jsonUnsafeNumber = (value: unknown): value is number => typeof value === 'number' && (!Number.isFinite(value) || Object.is(value, -0))
const jsName = (value: unknown) => Object.is(value, -0) ? '-0' : String(value)

// Paths of the values JSON drops or rewrites (undefined, NaN, infinities, -0), with each value as JS prints it.
function jsonUnsafe(value: unknown, at: string, found: { at: string; value: string }[] = []) {
    if (value === undefined || jsonUnsafeNumber(value)) found.push({ at, value: jsName(value) })
    else if (Array.isArray(value)) Array.from(value, (entry, index) => jsonUnsafe(entry, `${at}[${index}]`, found))
    else if (value !== null && typeof value === 'object') for (const [key, entry] of Object.entries(value)) jsonUnsafe(entry, `${at}.${key}`, found)
    return found
}

// A value with each part JSON cannot hold written as `{ js: '<as JS prints it>' }`.
function jsValue(value: unknown): unknown {
    if (value === undefined || jsonUnsafeNumber(value)) return { js: jsName(value) }
    if (Array.isArray(value)) return Array.from(value, jsValue)
    if (value !== null && typeof value === 'object') return Object.fromEntries(Object.entries(value).map(([key, entry]) => [key, jsValue(entry)]))
    return value
}

// What the start, request and output triggers left: the chat variables the request was built with, and the stored
// variables, author note and history after the send.
function triggerObservation(requestScriptstate: RecordValue | null, saved: { scriptstate?: unknown; note?: unknown; message: unknown[] }, storedMessages: unknown[]): TriggerObservation {
    const observation: TriggerObservation = { requestScriptstate, storedScriptstate: saved.scriptstate, storedNote: saved.note, storedMessages }
    const unsafe = Object.entries(observation).flatMap(([at, value]) => jsonUnsafe(value, at))
    return unsafe.length ? { ...observation, jsonUnsafe: unsafe } : observation
}

// The clock and random CBS (`{{unixtime}}`, `{{random}}`, ...) read Date.now and Math.random; a case that pins them hands the
// native route the same values. The tape throws once spent, so a draw outside the parse shows up as a failed send.
function pinEnvironment(entry: { id: string; now?: number; random?: number[]; timeZone?: string }, draws: number[] = []) {
    const spies: { mockRestore(): void }[] = []
    // A case with a time zone also pins `new Date()`, which the local time CBS reads, and the zone of `Date` and `Intl`.
    if (entry.timeZone !== undefined) {
        const zone = process.env.TZ
        process.env.TZ = entry.timeZone
        expect(new Intl.DateTimeFormat().resolvedOptions().timeZone, entry.id).toBe(entry.timeZone)
        vi.useFakeTimers({ toFake: ['Date'], now: entry.now })
        spies.push({ mockRestore: () => {
            vi.useRealTimers()
            if (zone === undefined) delete process.env.TZ
            else process.env.TZ = zone
        } })
    } else if (entry.now !== undefined) spies.push(vi.spyOn(Date, 'now').mockReturnValue(entry.now))
    if (entry.random) {
        const tape = [...entry.random]
        spies.push(vi.spyOn(Math, 'random').mockImplementation(() => {
            const value = tape.shift()
            if (value === undefined) throw new Error(`${entry.id}: random tape spent`)
            draws.push(value)
            return value
        }))
    }
    return () => { for (const spy of spies) spy.mockRestore() }
}

afterEach(() => {
    for (const dispose of startup.dispose.splice(0)) dispose()
    loreSetter.assignments = null
    vi.unstubAllGlobals()
    vi.restoreAllMocks()
})

it('captures native preparation expectations from real sendChat and the built-in Ollama SDK', async () => {
    vi.spyOn(console, 'log').mockImplementation(() => {})
    vi.spyOn(console, 'debug').mockImplementation(() => {})
    const results: { id: string; database: unknown; body: string; prepared: unknown; output: unknown; effects: { assignedMessageIds: { index: number; id: string }[] }; loreObservation?: LoreObservation; triggerObservation?: TriggerObservation }[] = []
    const store = getPersistentDataStore()
    await store.open()
    for (const entry of fixtures.cases) {
        const database = normalizeDatabaseDefaults(databaseFor(entry))
        characterFormatUpdate(database.characters[0])
        deriveEffectivePresetMirrors(database)
        deriveEffectivePersonaMirrors(database)
        const { revision } = await store.replaceFromDatabase(database)
        setDatabase(projectCompleteScalableWorkingSet(database, 'character', revision, new Set(['character'])))
        selectedCharID.set(0)
        startup.ready = true
        for (const activate of startup.roots.splice(0)) activate()
        const runtime = getPersistentDataRuntime()
        await runtime.initializeActiveWorkingSet(getDatabase())
        await runtime.flushPendingDataLocally('synthetic-ollama-oracle')
        await immediate()
        await reload('synthetic-ollama-oracle-reload')
        expect(pluginV2.replacerbeforeRequest.size).toBe(0)
        expect(pluginV2.replacerafterRequest.size).toBe(0)
        expect(pluginV2.chatOutput.size).toBe(0)
        const root = (await store.readRoot()).value
        const preset = (await store.readPreset('preset-a'))!.value
        const character = (await store.readCharacter('character'))!.value
        const conversation = (await store.readConversation('character', 'conversation'))!.value
        const nativeDatabase = { ...root, botPresets: [preset], characters: [{ ...character, chats: [conversation] }] }
        const expectedIdAssignments = entry.expectedIdAssignments ?? []
        expect(conversation.message.flatMap((message, index) => message.chatId == null ? [index] : []), entry.id).toEqual(expectedIdAssignments)
        let request: { model: string; messages: { role: string; content: string }[]; stream: boolean } | undefined
        let body = ''
        const loreObservation = entry.observeLore ? { assignments: [] as { name: string; value: string }[],
            requestScriptstate: null as RecordValue | null } : undefined
        let triggerRequestScriptstate: RecordValue | null = null
        const draws: number[] = []
        let responseDrawStart = 0
        vi.stubGlobal('fetch', vi.fn(async (input: RequestInfo | URL, init?: RequestInit) => {
            const luaJson = luaJsonModuleResponse(input)
            if (luaJson) return luaJson
            expect(String(input)).toBe('http://synthetic-ollama.invalid:80/api/chat')
            expect(init?.method).toBe('POST')
            if (request) throw new Error('Unexpected repeated provider request')
            responseDrawStart = draws.length
            if (loreObservation) loreObservation.requestScriptstate = structuredClone(getDatabase({ snapshot: true }).characters[0].chats[0].scriptstate ?? {})
            if (entry.observeTriggers) triggerRequestScriptstate = structuredClone(getDatabase({ snapshot: true }).characters[0].chats[0].scriptstate ?? null)
            body = String(init?.body)
            request = JSON.parse(body) as typeof request
            return new Response(JSON.stringify({ model: 'synthetic-model', done: true, message: { role: 'assistant', content: entry.responseContent ?? 'Synthetic response.' } }), {
                status: 200, headers: { 'Content-Type': 'application/json' },
            })
        }))
        const restore = pinEnvironment(entry, draws)
        updateStartupExclusions(entry.startupExclusions ?? [])
        resetScriptCache()
        const sendStarted = Date.now()
        loreSetter.assignments = loreObservation?.assignments ?? null
        let sendEnded: number
        try {
            expect(await sendChat({ historyLimit: false }), entry.id).toBe(true)
            await runtime.flushPendingDataLocally('synthetic-ollama-oracle-result')
            sendEnded = Date.now()
        } finally {
            loreSetter.assignments = null
            updateStartupExclusions([])
            restore()
        }
        if (entry.id === 'regex-output-random') expect(draws).toEqual(entry.random)
        expect(request, entry.id).toBeDefined()
        expect(request!.model).toBe('synthetic-model')
        expect(request!.stream).toBe(false)
        expect(request).not.toHaveProperty('temperature')
        expect(request).not.toHaveProperty('max_tokens')
        const saved = (await store.readConversation('character', 'conversation'))!.value
        expect(saved.message.at(-1)?.data, entry.id).toBe(entry.expectedOutput ?? 'Synthetic response.')
        // A trigger that edits messages leaves another history; such a case carries no nullish IDs to assign.
        if (entry.triggerEditsMessages) expect(expectedIdAssignments, entry.id).toEqual([])
        else expect(saved.message).toHaveLength(conversation.message.length + 1)
        const assignedMessageIds: { index: number; id: string }[] = []
        for (const [index, original] of entry.triggerEditsMessages ? [] : [...conversation.message.entries()]) {
            const actual = saved.message[index]
            if (expectedIdAssignments.includes(index)) {
                // The complete TS send normalizes nullish IDs before prompt selection,
                // including disabled rows and rows preceding the reset marker.
                expect(original.chatId).toBeUndefined()
                expect(actual.chatId).toEqual(expect.any(String))
                expect(actual.chatId!.length).toBeGreaterThan(0)
                expect(actual).toEqual({ ...original, chatId: actual.chatId })
                assignedMessageIds.push({ index, id: actual.chatId! })
            } else expect(actual).toEqual(original)
        }
        expect(new Set(assignedMessageIds.map(message => message.id)).size).toBe(assignedMessageIds.length)
        if (assignedMessageIds.length) {
            await runtime.flushPendingDataLocally('synthetic-ollama-oracle-repeat-flush')
            const persisted = (await store.readConversation('character', 'conversation'))!.value
            expect(assignedMessageIds.map(({ index }) => persisted.message[index].chatId)).toEqual(assignedMessageIds.map(message => message.id))
        }
        const info = saved.message.at(-1)!.generationInfo!
        const active = getDatabase()
        const bound = saved.bindedPersona ? active.personas.find(persona => persona.id === saved.bindedPersona) : undefined
        results.push({ id: entry.id, database: nativeDatabase, body, effects: { assignedMessageIds },
            ...(loreObservation ? { loreObservation: { ...loreObservation, storedScriptstate: saved.scriptstate ?? {} } } : {}),
            // The output row, recorded under `output`, is the last stored message.
            ...(entry.observeTriggers ? { triggerObservation: triggerObservation(triggerRequestScriptstate, saved, saved.message.slice(0, -1)) } : {}),
            output: { ...outputObservation(saved.message.at(-1), sendStarted, sendEnded), responseDraws: draws.slice(responseDrawStart), preparationDraws: draws.slice(0, responseDrawStart) }, prepared: {
            messages: request!.messages,
            inputTokens: info.inputTokens, outputTokens: info.outputTokens, maxContext: info.maxContext,
            lastMemory: saved.lastMemory ?? null,
            characterName: active.characters[0].name,
            userName: bound?.name ?? active.username ?? 'User',
        } })
    }
    expect(results.map(result => result.id)).toEqual(fixtures.cases.map(entry => entry.id))
    const effects: { id: string; outcome: string; database: unknown; assignedIndices: number[]; storedMessages: unknown[]; sendEffects: unknown; requests: number; body: string | null; error: string | null; loreObservation?: LoreObservation; triggerObservation?: TriggerObservation }[] = []
    for (const entry of fixtures.effects) {
        const database = normalizeDatabaseDefaults(databaseFor({ ...entry, messages: entry.messages ?? fixtures.effectMessages }))
        characterFormatUpdate(database.characters[0])
        deriveEffectivePresetMirrors(database)
        deriveEffectivePersonaMirrors(database)
        const { revision } = await store.replaceFromDatabase(database)
        setDatabase(projectCompleteScalableWorkingSet(database, 'character', revision, new Set(['character'])))
        selectedCharID.set(0)
        const runtime = getPersistentDataRuntime()
        await runtime.initializeActiveWorkingSet(getDatabase())
        await runtime.flushPendingDataLocally('synthetic-ollama-effects-before')
        await immediate()
        await reload('synthetic-ollama-effects-reload')
        const original = (await store.readConversation('character', 'conversation'))!.value
        const rootBefore = (await store.readRoot()).value
        const characterBefore = (await store.readCharacter('character'))!.value
        const nativeDatabase = { ...rootBefore,
            botPresets: [(await store.readPreset('preset-a'))!.value],
            characters: [{ ...characterBefore, chats: [original] }] }
        const indices = original.message.flatMap((message, index) => message.chatId == null ? [index] : [])
        expect(indices, entry.id).toEqual(entry.expectedIdAssignments)
        expect(original.message[1].chatId, entry.id).toBeNull()
        expect(original.message[4].chatId, entry.id).toBeNull()
        const controller = new AbortController()
        let requests = 0
        let body: string | null = null
        const loreObservation = entry.observeLore ? { assignments: [] as { name: string; value: string }[],
            requestScriptstate: null as RecordValue | null } : undefined
        let triggerRequestScriptstate: RecordValue | null = null
        const unsubscribeStage = chatProcessStage.subscribe(stage => {
            if (stage === 3 && entry.outcome === 'abort-before-request') controller.abort()
        })
        vi.stubGlobal('fetch', vi.fn(async (input: RequestInfo | URL, init?: RequestInit) => {
            expect(String(input)).toBe('http://synthetic-ollama.invalid:80/api/chat')
            expect(init?.method).toBe('POST')
            requests += 1
            if (loreObservation) loreObservation.requestScriptstate = structuredClone(getDatabase({ snapshot: true }).characters[0].chats[0].scriptstate ?? {})
            if (entry.observeTriggers) triggerRequestScriptstate = structuredClone(getDatabase({ snapshot: true }).characters[0].chats[0].scriptstate ?? null)
            body ??= String(init?.body)
            if (entry.outcome === 'abort') {
                expect(init?.signal?.aborted).toBe(false)
                controller.abort()
                expect(init?.signal?.aborted).toBe(true)
                throw new DOMException('Synthetic caller abort', 'AbortError')
            }
            if (entry.outcome === 'provider-failure') {
                return new Response(JSON.stringify({ error: 'Synthetic provider failure.' }), {
                    status: entry.status ?? 500, headers: { 'Content-Type': 'application/json' },
                })
            }
            if (entry.outcome === 'network-failure') throw new TypeError('fetch failed')
            return new Response(JSON.stringify({ done: true, message: { role: 'assistant', content: 'Synthetic effect response.' } }), {
                status: 200, headers: { 'Content-Type': 'application/json' },
            })
        }))
        resetScriptCache()
        const sendStarted = Date.now()
        // The caller owns the reservation and releases it after a rejected send, as the chat screen does.
        const reservation = reserveGeneration()
        expect(reservation, entry.id).not.toBeNull()
        loreSetter.assignments = loreObservation?.assignments ?? null
        const sent = sendChat({ historyLimit: false, signal: controller.signal }, reservation!).finally(() => {
            reservation!.release()
            loreSetter.assignments = null
        })
        // The Ollama SDK rejects a non-OK response or a failed fetch, and the TS send lets that error reach its caller
        // without another attempt or a retry delay, whatever `requestRetrys` holds.
        let error: string | null = null
        if (['invalid-host', 'provider-failure', 'network-failure'].includes(entry.outcome)) {
            error = await sent.then(() => { throw new Error(`${entry.id} resolved`) }, (reason: unknown) => reason instanceof Error ? reason.message : String(reason))
            if (entry.outcome === 'provider-failure') expect(error, entry.id).toBe('Synthetic provider failure.')
        } else expect(await sent, entry.id).toBe(entry.outcome === 'success')
        expect(Date.now() - sendStarted, entry.id).toBeLessThan(1000)
        unsubscribeStage()
        expect(requests, entry.id).toBe(Number(!['context-failure', 'abort-before-request', 'invalid-host', 'stop'].includes(entry.outcome)))
        await runtime.flushPendingDataLocally('synthetic-ollama-effects-after')
        const sendEnded = Date.now()
        const saved = (await store.readConversation('character', 'conversation'))!.value
        const rootAfter = (await store.readRoot()).value
        const characterAfter = (await store.readCharacter('character'))!.value
        const interaction = characterAfter.lastInteraction
        // Each send effect as stored after the attempt: the statics counter, the interaction time and the memory marker.
        const sendEffects = {
            statics: { before: rootBefore.statics ?? null, after: rootAfter.statics ?? null },
            lastInteraction: { before: characterBefore.lastInteraction ?? null,
                afterWithinSend: typeof interaction === 'number' && interaction >= sendStarted && interaction <= sendEnded },
            lastMemory: { before: original.lastMemory ?? null, after: saved.lastMemory ?? null },
            output: entry.outcome === 'success' ? outputObservation(saved.message.at(-1), sendStarted, sendEnded) : null,
        }
        expect(saved.message, entry.id).toHaveLength(original.message.length + Number(entry.outcome === 'success'))
        const assigned = indices.map(index => saved.message[index].chatId)
        expect(assigned.every(id => typeof id === 'string' && id.length > 0), entry.id).toBe(true)
        expect(new Set(assigned).size, entry.id).toBe(indices.length)
        for (const [index, row] of original.message.entries()) {
            expect(saved.message[index], entry.id).toEqual(indices.includes(index) ? { ...row, chatId: saved.message[index].chatId } : row)
        }
        effects.push({ id: entry.id, outcome: entry.outcome, database: nativeDatabase, assignedIndices: indices,
            storedMessages: saved.message.slice(0, original.message.length), sendEffects, requests, body, error,
            ...(loreObservation ? { loreObservation: { ...loreObservation, storedScriptstate: saved.scriptstate ?? {} } } : {}),
            ...(entry.observeTriggers ? { triggerObservation: triggerObservation(triggerRequestScriptstate, saved, saved.message) } : {}) })
        if (entry.outcome === 'invalid-host') continue
        // A later complete send reuses the published identities even after a failed/aborted send; a stopping trigger
        // stops only while its variable is unset, so the retry sends.
        vi.stubGlobal('fetch', vi.fn(async () => new Response(JSON.stringify({ done: true, message: { role: 'assistant', content: 'Synthetic retry.' } }), {
            status: 200, headers: { 'Content-Type': 'application/json' },
        })))
        resetScriptCache()
        expect(await sendChat({ historyLimit: false }), entry.id).toBe(entry.outcome !== 'context-failure')
        await runtime.flushPendingDataLocally('synthetic-ollama-effects-retry')
        const retried = (await store.readConversation('character', 'conversation'))!.value
        expect(retried.message.slice(0, original.message.length), entry.id).toEqual(saved.message.slice(0, original.message.length))
    }
    expect(effects.map(result => result.id)).toEqual(fixtures.effects.map(entry => entry.id))
    const variableValues = await captureVariableValues()
    const output = process.env.RISUNEST_OLLAMA_PREPARATION_ORACLE
    if (output) writeFileSync(output, JSON.stringify({ schema: 1, fixtureSha256: createHash('sha256').update(fixtureSource).digest('hex'), cases: results, effects, variableValues }, null, 2) + '\n')
}, 120000)

// The session store's contract for each value a chat variable write can hand it: what the active session keeps, stores and
// reloads. Which of them a trigger produces is shown by the trigger rows (parsed JSON passes numbers, booleans, arrays and
// objects through, and `1e400` becomes Infinity). `undefinedOverValue` starts as a stored string.
const variableProbes: [string, unknown][] = [
    ['number', 1.5], ['zero', 0], ['negativeZero', -0], ['true', true], ['false', false], ['null', null],
    ['array', [1, 'x', null, [2], { a: 1 }]], ['object', { a: 1, b: [true] }],
    ['undefined', undefined], ['undefinedOverValue', undefined], ['NaN', NaN], ['Infinity', Infinity], ['-Infinity', -Infinity],
    ['nestedUnsafe', [NaN, Infinity, undefined, -0, { u: undefined, n: NaN }]],
]

// Each probe as the session holds it, as the store returns it after the flush, and after a reload of the stored records.
// The store here is IndexedDB, which keeps structured clones; `nativeJson` is the JSON text the native store receives.
async function captureVariableValues() {
    const store = getPersistentDataStore()
    const database = normalizeDatabaseDefaults(databaseFor({ id: 'variable-values',
        conversation: { scriptstate: { preserve: 'value', $undefinedOverValue: 'before' } } }))
    characterFormatUpdate(database.characters[0])
    deriveEffectivePresetMirrors(database)
    deriveEffectivePersonaMirrors(database)
    const { revision } = await store.replaceFromDatabase(database)
    setDatabase(projectCompleteScalableWorkingSet(database, 'character', revision, new Set(['character'])))
    selectedCharID.set(0)
    const runtime = getPersistentDataRuntime()
    await runtime.initializeActiveWorkingSet(getDatabase())
    await runtime.flushPendingDataLocally('synthetic-variable-values')
    await immediate()
    await reload('synthetic-variable-values-reload')
    const live = () => getDatabase().characters[0]
    // A send holds the conversation this way while its trigger commits variables to the active session.
    const lease = await acquireCompleteConversation('synthetic-variable-values', captureSelectedConversationTarget())
    const read = (state: unknown) => {
        const record = (state ?? {}) as RecordValue
        return Object.fromEntries(variableProbes.map(([name]) => [name, Object.hasOwn(record, '$' + name) ? { value: record['$' + name] } : null]))
    }
    let inMemory: ReturnType<typeof read>
    try {
        const character = live()
        expect(peekActiveConversationSession()?.matchesConversation(character.chaId, character.chats[character.chatPage])).toBe(true)
        for (const [name, value] of variableProbes) setConversationChatVariable(character.chaId, character.chats[character.chatPage], name, value as string)
        inMemory = read(live().chats[live().chatPage].scriptstate)
    } finally {
        lease.release()
    }
    await runtime.flushPendingDataLocally('synthetic-variable-values-written')
    const stored = read((await store.readConversation('character', 'conversation'))!.value.scriptstate)
    expect(stored.number).toEqual({ value: 1.5 })
    await reload('synthetic-variable-values-reloaded')
    const reloaded = read(live().chats[live().chatPage].scriptstate)
    const shown = (entry: { value: unknown } | null) => entry ? { value: jsValue(entry.value) } : { absent: true }
    return variableProbes.map(([name, value]) => ({
        name, set: jsValue(value), inMemory: shown(inMemory[name]), stored: shown(stored[name]), reloaded: shown(reloaded[name]),
        nativeJson: JSON.stringify(prepareNativePersistenceValue(inMemory[name] ? { ['$' + name]: inMemory[name].value } : {})),
    }))
}
