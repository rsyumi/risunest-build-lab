import { expect, it, vi } from 'vitest'
import 'fake-indexeddb/auto'
import { readFileSync, realpathSync, writeFileSync } from 'node:fs'
import { dirname, relative, resolve } from 'node:path'
import { createHash } from 'node:crypto'
import { createRequire } from 'node:module'
import { setImmediate as immediate } from 'node:timers/promises'

const observation = vi.hoisted(() => {
    const state = {
        capture: null as null | ((stage: string, value: unknown) => void),
        assignments: null as null | { name: string; value: string }[],
        faults: [] as { stage: string; kind: string }[],
        overflow: false,
        roots: [] as (() => void)[],
        disposers: [] as (() => void)[],
        ready: false,
    }
    function fault(stage: string, kind: string) {
        if (state.faults.length < 32) state.faults.push({ stage, kind })
        else state.overflow = true
    }
    function capture(stage: string, value: unknown) {
        try { state.capture?.(stage, value) }
        catch { fault(stage, 'observer-failed') }
    }
    function watch<T>(stage: string, promise: Promise<T>, project: (value: T) => unknown = value => value): Promise<T> {
        void promise.then(value => {
            try { capture(`${stage}:return`, project(value)) }
            catch { fault(stage, 'observer-projection-failed') }
        }, error => capture(`${stage}:throw`, error))
        return promise
    }
    return { state, fault, capture, watch }
})

vi.mock('svelte/internal/client', async original => {
    const actual = await original<typeof import('svelte/internal/client')>()
    return { ...actual, effect_root: (fn: () => void) => {
        if (observation.state.ready) return actual.effect_root(fn)
        let dispose: (() => void) | undefined
        observation.state.roots.push(() => {
            dispose = actual.effect_root(fn)
            observation.state.disposers.push(dispose)
        })
        return () => dispose?.()
    } } satisfies typeof actual
})

vi.mock('../../src/ts/process/selectedChatVariables', async original => {
    const actual = await original<typeof import('../../src/ts/process/selectedChatVariables')>()
    return { ...actual, setSelectedChatVariable: (...args: Parameters<typeof actual.setSelectedChatVariable>) => {
        const result = actual.setSelectedChatVariable(...args)
        observation.state.assignments?.push({ name: args[0], value: args[1] })
        return result
    } }
})

vi.mock('../../src/ts/process/generationResponseApplication', async original => {
    const actual = await original<typeof import('../../src/ts/process/generationResponseApplication')>()
    return { ...actual, applyGenerationResponse: (options: Parameters<typeof actual.applyGenerationResponse>[0]) => {
        const callbacks = options.callbacks
        observation.capture('consumer:enter', { response: options.response, continue: options.continueGeneration })
        const forwarded: typeof options = { ...options, callbacks: { ...callbacks,
            processOutput: (...args) => {
                observation.capture('process-output:enter', { data: args[0], index: args[1] })
                try { return observation.watch('process-output', callbacks.processOutput(...args)) }
                catch (error) { observation.capture('process-output:throw', error); throw error }
            },
            reformatContent: data => {
                observation.capture('fallback-format:enter', data)
                try {
                    const result = callbacks.reformatContent(data)
                    observation.capture('fallback-format:return', result)
                    return result
                } catch (error) { observation.capture('fallback-format:throw', error); throw error }
            },
            markResponseApplied: () => {
                const result = callbacks.markResponseApplied()
                // This synchronous snapshot precedes the current-chat parser.
                try { observation.capture('response:applied', options.operation.getTargetChat()?.message) }
                catch { observation.fault('response:applied', 'observer-read-failed') }
                return result
            },
            runCurrentChatParser: chat => {
                observation.capture('current-chat:enter', chat.message)
                const result = callbacks.runCurrentChatParser(chat)
                observation.capture('current-chat:return', result.message)
                return result
            },
        } }
        try { return observation.watch('consumer', actual.applyGenerationResponse(forwarded), result => result?.result ?? null) }
        catch (error) { observation.capture('consumer:throw', error); throw error }
    } } satisfies typeof actual
})

import { deriveEffectivePresetMirrors, normalizeDatabaseDefaults, setDatabase, getDatabase, type Database, type Message } from '../../src/ts/storage/database.svelte'
import { deriveEffectivePersonaMirrors } from '../../src/ts/storage/effectiveIdentityState'
import { selectedCharID } from '../../src/ts/stores.svelte'
import { characterFormatUpdate } from '../../src/ts/characters'
import { getPersistentDataStore } from '../../src/ts/storage/persistentDataStoreFactory'
import { getPersistentDataRuntime } from '../../src/ts/storage/persistentDataRuntime.svelte'
import { projectCompleteScalableWorkingSet } from '../../src/ts/storage/workingSetCatalog'
import { pluginV2 } from '../../src/ts/plugins/plugins.svelte'
import { sendChat } from '../../src/ts/process/index.svelte'
import * as requestModule from '../../src/ts/process/request/request'
import { reserveGeneration, isGenerationReserved } from '../../src/ts/process/generationState'
import { NativeCommitTransport, encodeNativeCommit, type CommitEnvelope, type CommitTransportDependencies } from '../../src/ts/storage/nativeCommitTransport'
import { setConversationChatVariable } from '../../src/ts/process/selectedChatVariables'
import { resetScriptCache } from '../../src/ts/process/scripts'
import { updateStartupExclusions } from '../../src/ts/storage/deviceSettings'
import type { RecoveryExclusion } from '../../src/ts/storage/startupExclusions'
import { prepareNativePersistenceValue } from '../../src/ts/storage/nativePersistenceValue'

type Data = Record<string, unknown>
type ResponseCase = {
    id: string
    content: unknown
    thinking?: string
    continue?: boolean
    route: 'success' | 'fallback-error' | 'coercion-error' | 'adapter-error'
    adapterOutput?: string
    output: string | null
    nativeOutput?: string
    earlyOutput?: string
    root?: Data
    preset?: Data
    character?: Data
    conversation?: Data
    messages?: Message[]
    startupExclusions?: RecoveryExclusion[]
    random?: number[]
    excludedFromNativeAdmission?: boolean
    nativeUnsupported?: string
    midflightVariables?: Record<string, string>
    midflightDefaults?: { characterDefaultVariables?: string; presetTemplateDefaultVariables?: string }
    observeLore?: boolean
}
type Corpus = { schema: number; expectationStatus: string; continuePrefix: string; cases: ResponseCase[] }
type Baseline = { root: Data; preset: Data; character: Data; conversation: Data; messages: Message[] }
type Event = { stage: string; value: unknown }
type CaseResult = { id: string; excludedFromNativeAdmission: boolean; events: Event[]; observations: Data }
const sourcePaths = [
    'benchmarks/generation-send/ollama-owner-response-oracle.test.ts',
    'tests/fixtures/native-generation/ollama-owner-response.json',
    'tests/fixtures/native-generation/ollama-text.json',
    'src/ts/process/index.svelte.ts', 'src/ts/process/request/request.ts',
    'src/ts/process/stringlize.ts', 'src/ts/process/generationResponseApplication.ts',
    'src/ts/process/generationState.ts', 'src/ts/process/selectedChatVariables.ts', 'src/ts/process/scripts.ts', 'src/ts/parser/parser.svelte.ts',
    'src/ts/storage/database.svelte.ts', 'src/ts/storage/effectiveIdentityState.ts',
    'src/ts/storage/persistentDataRuntime.ts', 'src/ts/storage/persistentDataRuntime.svelte.ts',
    'src/ts/storage/indexedDbPersistentDataStore.ts',
    'src/ts/storage/nativePersistenceValue.ts', 'src/ts/storage/nativeCommitTransport.ts',
    'pnpm-lock.yaml', 'vitest.config.ts', 'tests/vitestShared.ts', 'vitest.setup.ts',
]
const corpusSource = readFileSync(resolve(sourcePaths[1]), 'utf8')
const corpus = JSON.parse(corpusSource) as Corpus
const baseline = JSON.parse(readFileSync(resolve(sourcePaths[2]), 'utf8')) as Baseline
const caseIds = ['text-trim', 'text-empty', 'text-pair', 'text-high-unit', 'text-low-unit',
    'array-coercion', 'array-marker', 'array-shadowed-tostring', 'thinking-coerces-number',
    'number-without-thinking', 'text-cbs', 'continue-array', 'continue-high-unit',
    'output-order-modes', 'output-excluded', 'output-index-cbs', 'output-random',
    'output-tokens', 'output-move-newline', 'output-invalid-skip', 'output-static-unsupported',
    'output-current-chat-write', 'output-late-unsupported', 'output-late-fragment-assembly', 'output-midflight-variable', 'output-selected-module-fresh-defaults']

// Traverse synthetic data without invoking Array/Object string coercion or JSON hooks.
function copyValue(value: unknown, depth = 0, budget = { remaining: 20000 }): unknown {
    if (--budget.remaining < 0 || depth > 24) throw new Error('Synthetic observation exceeded its bound')
    if (value === null || typeof value !== 'object') {
        if (typeof value === 'function' || typeof value === 'symbol' || typeof value === 'bigint') {
            throw new Error('Unsupported synthetic observation')
        }
        return value
    }
    if (Array.isArray(value)) return value.map(item => copyValue(item, depth + 1, budget))
    return Object.fromEntries(Object.keys(value).map(key => [key, copyValue((value as Data)[key], depth + 1, budget)]))
}

function textObservation(value: string) {
    return { text: value, units: Array.from({ length: value.length }, (_, index) => value.charCodeAt(index)) }
}

function errorObservation(error: unknown) {
    return error instanceof Error
        ? { name: error.name, message: textObservation(error.message) }
        : { thrown: copyValue(error) }
}

function assertCleanLedger() {
    expect(observation.state.faults, 'Independent request/observer fault ledger').toEqual([])
    expect(observation.state.overflow, 'Independent fault ledger overflow').toBe(false)
}

function installedRuntimeIdentity() {
    const resolver = createRequire(import.meta.url)
    function installed(name: string, entrySpecifier: string) {
        const packagePath = realpathSync(resolver.resolve(`${name}/package.json`))
        const entryPath = realpathSync(resolver.resolve(entrySpecifier))
        const packageBytes = readFileSync(packagePath)
        const metadata = JSON.parse(packageBytes.toString('utf8')) as { name: string; version: string }
        expect(metadata.name).toBe(name)
        expect(metadata.version).toEqual(expect.any(String))
        const entry = relative(dirname(packagePath), entryPath).replaceAll('\\', '/')
        expect(entry.startsWith('../'), 'Installed entry must belong to its resolved package').toBe(false)
        return { name, version: metadata.version, specifier: entrySpecifier, entry,
            packageSha256: createHash('sha256').update(packageBytes).digest('hex'),
            entrySha256: createHash('sha256').update(readFileSync(entryPath)).digest('hex') }
    }
    // request.ts imports this explicit file, so no CJS/browser condition substitution is inferred.
    const sdk = installed('ollama', 'ollama/dist/browser.mjs')
    const svelte = installed('svelte', 'svelte/internal/client')
    const environment = installed('happy-dom', 'happy-dom')
    const happyDom = (window as unknown as { happyDOM?: { waitUntilComplete?: unknown } }).happyDOM
    expect(typeof happyDom?.waitUntilComplete, 'The real happy-dom environment must be active').toBe('function')
    return { node: process.version, v8: process.versions.v8, sdk, svelteClient: svelte,
        environment: { ...environment, windowClass: window.constructor.name,
            userAgent: window.navigator.userAgent, happyDomApiPresent: true, configuredSvelteConditions: ['browser'] } }
}

function databaseFor(entry: ResponseCase): Database {
    const messages = structuredClone(entry.messages ?? baseline.messages)
    if (entry.continue) messages.push({ role: 'char', data: corpus.continuePrefix, chatId: 'continue-base', saying: 'character', time: 100 })
    const fallbackModels = { model: [], memory: [], emotion: [], translate: [], otherAx: [] }
    return structuredClone({ ...baseline.root, ...entry.root,
        requestRetrys: 0, useStreaming: false, fallbackWhenBlankResponse: false, fallbackModels,
        removeIncompleteResponse: false, ttsAutoSpeech: false, autoContinueChat: false,
        autoContinueMinTokens: 0, igpPrompt: '', inlayErrorResponse: false,
        botPresets: [{ ...baseline.preset, ...entry.preset, useStreaming: false, fallbackModels, fallbackWhenBlankResponse: false }],
        characters: [{ ...baseline.character, ...entry.character, chats: [{ ...baseline.conversation, ...entry.conversation, message: messages }] }],
    }) as unknown as Database
}

async function nativeSubmission(rows: Message[], expectedText: string | null) {
    const input: CommitEnvelope = { commit: { expectedRevision: 1, conversations: [{
        type: 'replace-range', characterId: 'character', conversationId: 'conversation',
        start: 0, deleteCount: rows.length, messages: rows,
    }] }, assetAliases: [] }
    const original = copyValue(input)
    const normalized = prepareNativePersistenceValue(input)
    const bytes = await encodeNativeCommit(input)
    const encoded = JSON.parse(new TextDecoder().decode(bytes)) as CommitEnvelope
    const submissions: { command: string; value: unknown }[] = []
    const boundaryOnlyReceipt = { revision: 2 }
    const dependencies: CommitTransportDependencies = {
        windows: () => false, shared: () => undefined, encode: encodeNativeCommit,
        invoke: async <T>(command: string, args?: Record<string, unknown> | Uint8Array): Promise<T> => {
            submissions.push({ command, value: copyValue(args) })
            return boundaryOnlyReceipt as T
        },
    }
    await new NativeCommitTransport(dependencies).commit(input)
    expect(submissions).toEqual([{ command: 'pds_commit', value: normalized }])
    expect(encoded).toEqual(normalized)
    expect(copyValue(input), 'Native preparation must not alter the JS source').toEqual(original)
    const mutation = encoded.commit.conversations![0]
    expect(mutation.type).toBe('replace-range')
    if (mutation.type !== 'replace-range') throw new Error('Unexpected encoded mutation')
    if (expectedText !== null) expect(mutation.messages.at(-1)!.data).toBe(expectedText)
    return { proof: 'native submission only; no SQLite commit or receipt', byteLength: bytes.length,
        payloadSha256: createHash('sha256').update(bytes).digest('hex'),
        messages: copyValue(mutation.messages), texts: mutation.messages.map(message => textObservation(message.data)) }
}

it('characterizes synthetic responses through real Ollama Send, application and persistence', async () => {
    const results: CaseResult[] = []
    const sourceHashes = Object.fromEntries(sourcePaths.map(path => [path,
        createHash('sha256').update(readFileSync(resolve(path))).digest('hex')]))
    let complete = false
    let primaryFailure: unknown
    let active: ResponseCase | undefined
    let requestCount = 0
    let responseDrawStart = 0
    let currentDraws: number[] = []
    let requestEntered: (() => void) | undefined
    let responseReleased: Promise<void> | undefined
    let wire: unknown
    let runtimeIdentity: ReturnType<typeof installedRuntimeIdentity> | undefined
    const fetchBoundary: typeof fetch = async (input, init) => {
        const url = typeof input === 'string' ? input : input instanceof URL ? input.href : input.url
        if (!active || url !== 'http://synthetic-ollama.invalid:80/api/chat' || init?.method !== 'POST'
            || requestCount !== 0 || typeof init.body !== 'string') {
            observation.fault(active?.id ?? 'outside-case', 'unexpected-request')
            throw new Error('Blocked unexpected synthetic oracle request')
        }
        responseDrawStart = currentDraws.length
        requestCount++
        try { wire = JSON.parse(init.body) }
        catch {
            observation.fault(active.id, 'invalid-request-body')
            throw new Error('Invalid synthetic oracle request body')
        }
        if (active.midflightVariables || active.midflightDefaults) {
            requestEntered!()
            await responseReleased
        }
        return new Response(JSON.stringify({ model: 'synthetic-model', done: true,
            message: { role: 'assistant', content: active.content, thinking: active.thinking ?? '' } }), {
            status: 200, headers: { 'Content-Type': 'application/json' },
        })
    }
    vi.stubGlobal('fetch', fetchBoundary)
    vi.spyOn(console, 'log').mockImplementation(() => {})
    vi.spyOn(console, 'debug').mockImplementation(() => {})
    const store = getPersistentDataStore()
    try {
        runtimeIdentity = installedRuntimeIdentity()
        const originalRequest = requestModule.requestChatData
        vi.spyOn(requestModule, 'requestChatData').mockImplementation(function (
            this: unknown, ...args: Parameters<typeof originalRequest>
        ): ReturnType<typeof originalRequest> {
            observation.capture('request:enter', { useStreaming: args[0].useStreaming, continue: args[0].continue })
            try {
                const result: ReturnType<typeof originalRequest> = Reflect.apply(originalRequest, this, args)
                return observation.watch('request', result)
            } catch (error) { observation.capture('request:throw', error); throw error }
        })
        expect(corpus.schema).toBe(1)
        expect(corpus.cases.map(entry => entry.id)).toEqual(caseIds)
        await store.open()
        for (const entry of corpus.cases) {
            assertCleanLedger()
            const result: CaseResult = { id: entry.id, excludedFromNativeAdmission: entry.continue === true || entry.excludedFromNativeAdmission === true,
                events: [], observations: {} }
            results.push(result)
            const errors = new Map<string, unknown[]>()
            observation.state.capture = (stage, value) => {
                if (result.events.length >= 128) throw new Error('Too many synthetic observation events')
                if (stage.endsWith(':throw')) {
                    errors.set(stage, [...(errors.get(stage) ?? []), value])
                    result.events.push({ stage, value: errorObservation(value) })
                } else result.events.push({ stage, value: copyValue(value) })
            }
            const events = (stage: string) => result.events.filter(event => event.stage === stage)
            const first = (stage: string) => result.events.findIndex(event => event.stage === stage)
            const database = normalizeDatabaseDefaults(databaseFor(entry))
            characterFormatUpdate(database.characters[0])
            deriveEffectivePresetMirrors(database)
            deriveEffectivePersonaMirrors(database)
            const { revision } = await store.replaceFromDatabase(database)
            setDatabase(projectCompleteScalableWorkingSet(database, 'character', revision, new Set(['character'])))
            selectedCharID.set(0)
            observation.state.ready = true
            for (const activate of observation.state.roots.splice(0)) activate()
            const runtime = getPersistentDataRuntime()
            await runtime.initializeActiveWorkingSet(getDatabase())
            await runtime.flushPendingDataLocally('synthetic-owner-response-before')
            await immediate()
            expect(pluginV2.replacerbeforeRequest.size).toBe(0)
            expect(pluginV2.replacerafterRequest.size).toBe(0)
            expect(pluginV2.chatOutput.size).toBe(0)
            const effective = getDatabase()
            expect(effective.aiModel).toBe('ollama-hosted')
            expect(effective.useStreaming).toBe(false)
            expect(effective.requestRetrys).toBe(0)
            expect(effective.fallbackWhenBlankResponse).toBe(false)
            expect(effective.fallbackModels.model).toEqual([])
            expect(effective.removeIncompleteResponse).toBe(false)
            expect(effective.ttsAutoSpeech).toBe(false)
            expect(effective.autoContinueChat).toBe(false)
            expect(effective.autoContinueMinTokens).toBe(0)
            expect(effective.igpPrompt).toBe('')
            expect(effective.characters[0].name).toBe('Synthetic character')
            expect(effective.characters[0].customscript).toEqual(entry.character?.customscript ?? [])
            expect(effective.characters[0].triggerscript).toEqual([])
            result.observations.profile = { aiModel: effective.aiModel, useStreaming: effective.useStreaming,
                requestRetrys: effective.requestRetrys, fallbackModels: copyValue(effective.fallbackModels),
                historyLimit: false, continue: entry.continue === true }
            const original = (await store.readConversation('character', 'conversation'))!.value
            result.observations.nativeUnsupported = entry.nativeUnsupported ?? null
            result.observations.before = copyValue(original.message)
            result.observations.database = copyValue({ ...(await store.readRoot()).value,
                botPresets: [(await store.readPreset('preset-a'))!.value],
                characters: [{ ...(await store.readCharacter('character'))!.value, chats: [original] }] })
            const reservation = reserveGeneration()
            expect(reservation, entry.id).not.toBeNull()
            if (!reservation) throw new Error('Synthetic oracle could not reserve generation')
            requestCount = 0
            wire = undefined
            active = entry
            resetScriptCache()
            updateStartupExclusions(entry.startupExclusions ?? [])
            const draws: number[] = []
            currentDraws = draws
            responseDrawStart = 0
            const randomSpy = entry.random ? vi.spyOn(Math, 'random').mockImplementation(() => {
                const value = entry.random![draws.length]
                if (value === undefined) throw new Error(`${entry.id}: random tape spent`)
                draws.push(value)
                return value
            }) : undefined
            let releaseResponse: (() => void) | undefined
            const entered = new Promise<void>(resolve => { requestEntered = resolve })
            responseReleased = new Promise<void>(resolve => { releaseResponse = resolve })
            let sendError: unknown
            let returned: boolean | undefined
            try {
                const assignments = entry.observeLore ? [] as { name: string; value: string }[] : undefined
                observation.state.assignments = assignments ?? null
                const pending = sendChat({ historyLimit: false, continue: entry.continue === true }, reservation)
                const settledSend = pending.then(value => ({ ok: true as const, value }), error => ({ ok: false as const, error }))
                let harnessFailure: { error: unknown } | undefined
                try {
                    if (entry.midflightVariables || entry.midflightDefaults) {
                        const reached = await Promise.race([entered.then(() => true), settledSend.then(() => false)])
                        if (!reached) throw new Error('Send completed before held response')
                        expect(requestCount).toBe(1)
                        expect(events('process-output:enter')).toHaveLength(0)
                        const character = getDatabase().characters[0]
                        const chat = character.chats[character.chatPage]
                        for (const [key, value] of Object.entries(entry.midflightVariables ?? {})) {
                            expect(chat.scriptstate?.['$' + key], entry.id).not.toBe(value)
                            setConversationChatVariable(character.chaId, chat, key, value)
                        }
                        const activeDatabase = getDatabase()
                        if (entry.midflightDefaults?.characterDefaultVariables !== undefined) {
                            activeDatabase.characters[0].defaultVariables = entry.midflightDefaults.characterDefaultVariables
                        }
                        if (entry.midflightDefaults?.presetTemplateDefaultVariables !== undefined) {
                            const preset = activeDatabase.botPresets.find(value => value.id === 'preset-a')
                            expect(preset, entry.id).toBeDefined()
                            preset!.templateDefaultVariables = entry.midflightDefaults.presetTemplateDefaultVariables
                            deriveEffectivePresetMirrors(activeDatabase)
                        }
                        await runtime.flushPendingDataLocally('synthetic-owner-response-midflight')
                        const edited = (await store.readConversation('character', 'conversation'))!.value
                        const editedPreset = (await store.readPreset('preset-a'))!.value
                        const editedCharacter = (await store.readCharacter('character'))!.value
                        result.observations.midflight = { requestHeld: true, database: copyValue({ ...(await store.readRoot()).value,
                            botPresets: [editedPreset], characters: [{ ...editedCharacter, chats: [edited] }] }) }
                        for (const [key, value] of Object.entries(entry.midflightVariables ?? {})) expect(edited.scriptstate?.['$' + key]).toBe(value)
                        if (entry.midflightDefaults?.characterDefaultVariables !== undefined) {
                            expect(editedCharacter.defaultVariables, entry.id).toBe(entry.midflightDefaults.characterDefaultVariables)
                        }
                        if (entry.midflightDefaults?.presetTemplateDefaultVariables !== undefined) {
                            expect(editedPreset.templateDefaultVariables, entry.id).toBe(entry.midflightDefaults.presetTemplateDefaultVariables)
                        }
                    }
                } catch (error) {
                    harnessFailure = { error }
                } finally {
                    releaseResponse?.()
                    const outcome = await settledSend
                    if (outcome.ok === true) {
                        returned = outcome.value
                        observation.capture('send:return', returned)
                    } else {
                        sendError = outcome.error
                        observation.capture('send:throw', sendError)
                    }
                    observation.state.assignments = null
                }
                if (harnessFailure) throw harnessFailure.error
                if (assignments) result.observations.loreAssignments = copyValue(assignments)
                // The awaited Send has completed its own finally, before caller cleanup or manual flushing.
                const memory = copyValue(getDatabase().characters[0].chats[0].message) as Message[]
                const settled = (await store.readConversation('character', 'conversation'))!
                result.observations.sendSettled = { returned, error: sendError === undefined ? null : errorObservation(sendError),
                    inMemory: memory, durable: copyValue(settled.value.message), revision: settled.revision,
                    reservationStillCurrent: reservation.isCurrent() }
                await runtime.flushPendingDataLocally('synthetic-owner-response-explicit-after')
                const flushed = (await store.readConversation('character', 'conversation'))!
                result.observations.afterExplicitFlush = { messages: copyValue(flushed.value.message), revision: flushed.revision }
                result.observations.scriptstate = { before: copyValue(original.scriptstate), after: copyValue(flushed.value.scriptstate) }
                result.observations.request = { count: requestCount, body: copyValue(wire) }
                result.observations.nativeSubmission = await nativeSubmission(flushed.value.message,
                    entry.output === null ? null : entry.nativeOutput ?? entry.output)
                assertCleanLedger()
                expect(requestCount, entry.id).toBe(1)
                expect(wire).toMatchObject({ model: 'synthetic-model', stream: false })
                expect(events('request:enter')).toHaveLength(1)
                expect(events('request:enter')[0].value).toEqual({ useStreaming: true, continue: entry.continue === true })
                const applied = entry.output !== null
                const expectedLength = original.message.length + Number(applied && !entry.continue)
                for (const rows of [memory, settled.value.message, flushed.value.message]) {
                    expect(rows, entry.id).toHaveLength(expectedLength)
                    const unchanged = entry.continue && applied ? original.message.length - 1 : original.message.length
                    expect(rows.slice(0, unchanged), entry.id).toEqual(original.message.slice(0, unchanged))
                    if (applied) {
                        const output = rows.at(-1)!
                        expect(output.data, entry.id).toBe(entry.output)
                        expect(output.role).toBe('char')
                        expect(output.saying).toBe('character')
                        expect(output.chatId).toEqual(expect.any(String))
                        expect(output.chatId!.length).toBeGreaterThan(0)
                        expect(original.message.some(message => message.chatId === output.chatId)).toBe(false)
                        expect(output.time).toEqual(expect.any(Number))
                        expect(output.generationInfo).toEqual(expect.objectContaining({ generationId: output.chatId }))
                        if (entry.route === 'fallback-error') {
                            expect(output.generationInfo?.stageTiming).toMatchObject({ stage3: 0, stage4: 0 })
                        }
                        expect(output.promptInfo).toBeDefined()
                    }
                }
                result.observations.texts = { input: typeof entry.content === 'string' ? textObservation(entry.content) : null,
                    memory: memory.map(message => textObservation(message.data)),
                    durable: settled.value.message.map(message => textObservation(message.data)),
                    afterExplicitFlush: flushed.value.message.map(message => textObservation(message.data)) }
                if (entry.route === 'adapter-error') {
                    expect(events('request:throw')).toHaveLength(1)
                    expect(events('consumer:enter')).toHaveLength(0)
                    expect(sendError).toBe(errors.get('request:throw')![0])
                    expect(sendError).toBeInstanceOf(TypeError)
                } else {
                    expect(events('request:return')).toHaveLength(1)
                    const adapter = events('request:return')[0].value as { type: string; result: unknown }
                    expect(adapter.type).toBe('success')
                    expect(adapter.result).toEqual(entry.adapterOutput ?? entry.content)
                    expect(events('consumer:enter')).toHaveLength(1)
                    expect(events('consumer:enter')[0].value).toEqual({ response: adapter, continue: entry.continue === true })
                    expect((events('process-output:enter')[0].value as { data: unknown }).data).toEqual(adapter.result)
                    if (entry.route === 'success') {
                        expect(sendError).toBeUndefined()
                        expect(returned).toBe(true)
                        expect(events('consumer:return')).toHaveLength(1)
                        expect(events('consumer:return')[0].value).toBe(entry.earlyOutput ?? entry.output)
                        const outputs = events('process-output:return').map(event => (event.value as { data: string }).data)
                        expect(outputs.at(-1)).toBe(entry.earlyOutput ?? entry.output)
                        expect(outputs).toHaveLength(entry.continue ? 2 : 1)
                    } else {
                        expect(returned).toBeUndefined()
                        expect(events('process-output:throw')).toHaveLength(1)
                        expect(events('consumer:throw')).toHaveLength(1)
                        expect(sendError).toBe(errors.get('consumer:throw')![0])
                        expect(sendError).toBeInstanceOf(TypeError)
                        if (entry.route === 'fallback-error') {
                            expect(sendError).toBe(errors.get('process-output:throw')![0])
                            expect(events('fallback-format:return')[0].value).toBe(entry.output)
                            expect(first('response:applied')).toBeLessThan(first('consumer:throw'))
                        } else {
                            expect(sendError).not.toBe(errors.get('process-output:throw')![0])
                            expect(events('fallback-format:enter')).toHaveLength(0)
                        }
                    }
                }
                expect(events('response:applied')).toHaveLength(Number(applied))
                if (entry.midflightVariables) {
                    for (const [key, value] of Object.entries(entry.midflightVariables)) expect(flushed.value.scriptstate?.['$' + key]).toBe(value)
                    expect(flushed.value.scriptstate?.preserve).toBe('value')
                }
                if (entry.id === 'output-index-cbs') {
                    expect((events('process-output:enter')[0].value as { index: number }).index).toBe(4)
                }
                if (entry.id === 'output-random') expect(draws).toEqual(entry.random)
                if (entry.earlyOutput !== undefined) {
                    expect((events('response:applied')[0].value as Message[]).at(-1)!.data).toBe(entry.earlyOutput)
                    expect(first('response:applied')).toBeLessThan(first('current-chat:enter'))
                    expect(flushed.value.scriptstate?.$mood).toBe('bright')
                }
                if (entry.id === 'text-cbs') {
                    const early = events('response:applied')[0].value as Message[]
                    expect(early.at(-1)!.data).toBe('Hello Synthetic character.')
                    expect((events('process-output:return')[0].value as { data: string }).data).toBe('Hello Synthetic character.')
                    expect(first('process-output:return')).toBeLessThan(first('response:applied'))
                    expect(first('response:applied')).toBeLessThan(first('current-chat:enter'))
                }
            } finally {
                result.observations.random = { supplied: entry.random ?? [], consumed: draws, responseDraws: draws.slice(responseDrawStart), preparationDraws: draws.slice(0, responseDrawStart) }
                releaseResponse?.()
                requestEntered = undefined
                responseReleased = undefined
                randomSpy?.mockRestore()
                updateStartupExclusions([])
                active = undefined
                observation.state.assignments = null
                reservation.release()
                result.observations.callerReleased = !isGenerationReserved()
                assertCleanLedger()
            }
            expect(isGenerationReserved()).toBe(false)
            observation.state.capture = null
        }
        expect(results.map(result => result.id)).toEqual(caseIds)
        complete = true
    } catch (error) {
        primaryFailure = error
        throw error
    } finally {
        active = undefined
        observation.state.assignments = null
        observation.state.capture = null
        for (const dispose of observation.state.disposers.splice(0)) {
            try { dispose() } catch { observation.fault('root-disposal', 'cleanup-failed') }
        }
        vi.unstubAllGlobals()
        vi.restoreAllMocks()
        const output = process.env.RISUNEST_OLLAMA_OWNER_RESPONSE_ORACLE
        if (output) writeFileSync(output, JSON.stringify({ schema: 1,
            complete: complete && observation.state.faults.length === 0 && !observation.state.overflow,
            expectationStatus: corpus.expectationStatus, sourceHashes, runtime: runtimeIdentity ?? null,
            primaryFailure: primaryFailure === undefined ? null : errorObservation(primaryFailure),
            faultLedger: observation.state.faults, faultLedgerOverflow: observation.state.overflow,
            nativeProof: 'submission only; SQLite execution is a separate gate',
            nativeAdmission: 'unqualified; exclusion flags identify known controls only', cases: results }, null, 2) + '\n')
        assertCleanLedger()
    }
}, 120000)
