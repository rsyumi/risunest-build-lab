import { afterEach, expect, it, vi } from 'vitest'
import 'fake-indexeddb/auto'
import { createHash } from 'node:crypto'
import { readFileSync } from 'node:fs'
import { resolve } from 'node:path'
import { setImmediate as immediate } from 'node:timers/promises'
import { readOracleCases, settleOracle, type OracleSet } from './oracleFile'

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
// Fresh message and generation IDs come from the random source; a counter keeps the expected files stable.
const ids = vi.hoisted(() => ({ next: 0 }))
vi.mock('uuid', async (original) => {
    const actual = await original<typeof import('uuid')>()
    return { ...actual, v4: () => `00000000-0000-4000-8000-${String(++ids.next).padStart(12, '0')}` }
})
// The alert dialog is the UI boundary where a failed send reports its error text.
const alerts = vi.hoisted(() => ({ errors: [] as string[] }))
vi.mock('../../src/ts/alert', async (original) => {
    const actual = await original<typeof import('../../src/ts/alert')>()
    return { ...actual, alertError: (message: string | Error) => { alerts.errors.push(String(message)) } }
})
// The stage boundary, observed without changing it: what applyGenerationResponse receives and what it leaves in the
// conversation when it returns or throws.
const boundary = vi.hoisted(() => ({ record: null as null | { entry?: unknown; exit?: unknown } }))
vi.mock('../../src/ts/process/generationResponseApplication', async (original) => {
    type Module = typeof import('../../src/ts/process/generationResponseApplication')
    const actual = await original<Module>()
    type Options = Parameters<Module['applyGenerationResponse']>[0]
    const json = (value: unknown) => JSON.parse(JSON.stringify(value)) as unknown
    const conversation = (options: Options) => {
        const chat = options.operation.getTargetChat()
        return chat ? json(chat) : null
    }
    const applyGenerationResponse: Module['applyGenerationResponse'] = async (options) => {
        const record = boundary.record
        if (record) {
            // What the stage reads besides its arguments: the character the callbacks close over, the preset scripts,
            // the settings getters and the registered plugin callbacks.
            const { getDatabase } = await import('../../src/ts/storage/database.svelte')
            const { pluginV2 } = await import('../../src/ts/plugins/plugins.svelte')
            const database = getDatabase()
            const character = database.characters[0]
            const fields = ['chaId', 'name', 'type', 'firstMessage', 'alternateGreetings', 'emotionImages', 'inlayViewScreen',
                'viewScreen', 'customscript', 'triggerscript'] as const
            const response = options.response.type === 'streaming' ? { type: 'streaming' } : options.response
            record.entry = json({
                response, continue: options.continueGeneration, generationId: options.generationId,
                saying: options.sayingCharacterId, generationInfo: options.generationInfo, promptInfo: options.promptInfo,
                character: Object.fromEntries(fields.map((field) => [field, character[field] ?? null])),
                presetRegex: database.presetRegex ?? [],
                settings: { removeIncompleteResponse: options.removeIncompleteResponse(), ttsAutoSpeech: options.ttsAutoSpeech() },
                plugins: { editoutput: pluginV2.editoutput.size, chatOutput: pluginV2.chatOutput.size },
                conversation: conversation(options),
            })
        }
        try {
            const application = await actual.applyGenerationResponse(options)
            if (record) {
                record.exit = {
                    application: application
                        ? { result: application.result, emoChanged: application.emoChanged, resendChat: application.resendChat }
                        : null,
                    conversation: conversation(options),
                }
            }
            return application
        } catch (error) {
            if (record) record.exit = { error: error instanceof Error ? error.message : String(error), conversation: conversation(options) }
            throw error
        }
    }
    return { ...actual, applyGenerationResponse } satisfies Module
})

import { get } from 'svelte/store'
import { deriveEffectivePresetMirrors, normalizeDatabaseDefaults, setDatabase, getDatabase, type Database } from '../../src/ts/storage/database.svelte'
import { deriveEffectivePersonaMirrors } from '../../src/ts/storage/effectiveIdentityState'
import { CharEmotion, selectedCharID } from '../../src/ts/stores.svelte'
import { characterFormatUpdate } from '../../src/ts/characters'
import { getPersistentDataStore } from '../../src/ts/storage/persistentDataStoreFactory'
import { getPersistentDataRuntime } from '../../src/ts/storage/persistentDataRuntime.svelte'
import { projectCompleteScalableWorkingSet } from '../../src/ts/storage/workingSetCatalog'
import { pluginV2 } from '../../src/ts/plugins/plugins.svelte'
import { sendChat } from '../../src/ts/process/index.svelte'
import { resetScriptCache } from '../../src/ts/process/scripts'
import { isGenerationReserved, reserveGeneration } from '../../src/ts/process/generationState'

type RecordValue = Record<string, unknown>
type Reply = { value: string } | { suffix: string } | { null: true } | { undefined: true } | { throw: string }
type Case = {
    id: string
    root?: RecordValue; preset?: RecordValue; character?: RecordValue; conversation?: RecordValue; messages?: unknown[]
    continue?: boolean
    reply: { content: unknown; thinking?: string } | { multiline: string[] }
    hooks?: number
    replies?: Reply[]
    listeners?: number
    decline?: string
}
// Provider-envelope cases share the actual owner corpus, which is read and fingerprinted in full.
type OllamaCase = { id: string; oracleScope: 'provider-envelope' | 'owner-output'; content: unknown; thinking?: string; continue?: boolean }
type Base = { root: RecordValue; preset: RecordValue; character: RecordValue; conversation: RecordValue; messages: unknown[] }
type HookCall = { hook: number; data: string; reply: { value: string } | { null: true } | { undefined: true } | { throw: string } }

const now = 1767225600000
const applicationSet = readOracleCases<Case>('response', 'application')
const writeOllamaOnly = process.env.RISUNEST_OLLAMA_RESPONSE_ORACLE_WRITE === '1'
if (writeOllamaOnly && process.env.RUST_CORE_ORACLE_WRITE === '1') throw new Error('Ollama-only response capture requires RUST_CORE_ORACLE_WRITE unset')
// Fixtures read in place may carry CRLF in a checkout, so their hashes cover LF-normalized text.
const normalized = (path: string) => readFileSync(resolve(applicationSet.root, path), 'utf8').replaceAll('\r\n', '\n')
const baseSource = normalized('tests/fixtures/native-generation/ollama-text.json')
const base = JSON.parse(baseSource) as Base
const ollamaPath = 'tests/fixtures/native-generation/ollama-owner-response.json'
const ollamaSource = normalized(ollamaPath)
const ollamaCorpus = JSON.parse(ollamaSource) as { continuePrefix: string; cases: OllamaCase[] }
const providerEnvelopeIds = ['text-trim', 'text-empty', 'text-pair', 'text-high-unit', 'text-low-unit',
    'array-coercion', 'array-marker', 'array-shadowed-tostring', 'thinking-coerces-number',
    'number-without-thinking', 'text-cbs', 'continue-array', 'continue-high-unit']
const ownerOutputIds = ['output-order-modes', 'output-excluded', 'output-index-cbs', 'output-random',
    'output-tokens', 'output-move-newline', 'output-invalid-skip', 'output-static-unsupported',
    'output-current-chat-write', 'output-late-unsupported', 'output-late-fragment-assembly', 'output-midflight-variable', 'output-selected-module-fresh-defaults']
// Configured output and midflight owner cases belong to the dedicated actual owner oracle.
// This core response projection does not execute or qualify those owner scenarios.
const providerEnvelope = ollamaCorpus.cases.filter(entry => entry.oracleScope === 'provider-envelope')
const ownerOutput = ollamaCorpus.cases.filter(entry => entry.oracleScope === 'owner-output')
const ollamaSet: OracleSet<OllamaCase> = {
    root: applicationSet.root,
    casesPath: resolve(applicationSet.root, ollamaPath),
    expectedPath: resolve(applicationSet.root, 'tests/fixtures/rust-core/response/ollama.expected.json'),
    fixtureSha256: createHash('sha256').update(ollamaSource).digest('hex'),
    schema: 1,
    cases: providerEnvelope,
}

const tsSources = [
    'src/ts/process/generationResponseApplication.ts', 'src/ts/process/index.svelte.ts', 'src/ts/process/scripts.ts',
    'src/ts/process/currentChatParserPass.ts', 'src/ts/process/inlayScreen.ts', 'src/ts/process/triggers.ts',
    'src/ts/process/generationConversationOperation.ts', 'src/ts/responseVariants.ts', 'src/ts/util.ts',
    'src/ts/parser/parser.svelte.ts', 'src/ts/cbs.ts', 'src/ts/process/request/request.ts',
    'src/ts/process/request/openAI/requests.ts',
]

const json = <T>(value: T): T => JSON.parse(JSON.stringify(value)) as T
const fallbackModels = { model: [], memory: [], emotion: [], translate: [], otherAx: [] }

// Every knob after the stage is off, so the stored conversation is the stage's work alone.
function databaseFor(entry: Partial<Case>, messages: unknown[]): Database {
    return structuredClone({
        ...base.root,
        requestRetrys: 0, useStreaming: false, fallbackWhenBlankResponse: false, fallbackModels,
        removeIncompleteResponse: false, ttsAutoSpeech: false, autoContinueChat: false, autoContinueMinTokens: 0,
        igpPrompt: '', inlayErrorResponse: false, genTime: 1,
        ...entry.root,
        botPresets: [{ ...base.preset, useStreaming: false, fallbackModels, fallbackWhenBlankResponse: false, ...entry.preset }],
        characters: [{ ...base.character, ...entry.character, chats: [{ ...base.conversation, ...entry.conversation, message: messages }] }],
    }) as unknown as Database
}

function prepared(database: Database) {
    const result = normalizeDatabaseDefaults(database)
    characterFormatUpdate(result.characters[0])
    deriveEffectivePresetMirrors(result)
    deriveEffectivePersonaMirrors(result)
    return result
}

async function activate(database: Database, label: string) {
    const store = getPersistentDataStore()
    await store.open()
    const { revision } = await store.replaceFromDatabase(database)
    setDatabase(projectCompleteScalableWorkingSet(database, 'character', revision, new Set(['character'])))
    selectedCharID.set(0)
    startup.ready = true
    for (const start of startup.roots.splice(0)) start()
    const runtime = getPersistentDataRuntime()
    await runtime.initializeActiveWorkingSet(getDatabase())
    await runtime.flushPendingDataLocally(label)
    await immediate()
    return runtime
}

function ollamaReply(content: unknown, thinking?: string) {
    return new Response(JSON.stringify({ model: 'synthetic-model', done: true, message: { role: 'assistant', content, thinking: thinking ?? '' } }), {
        status: 200, headers: { 'Content-Type': 'application/json' },
    })
}

function openAIReply(contents: string[]) {
    return new Response(JSON.stringify({
        choices: contents.map((content, index) => ({ index, message: { role: 'assistant', content }, finish_reason: 'stop' })),
    }), { status: 200, headers: { 'Content-Type': 'application/json' } })
}

afterEach(() => {
    for (const dispose of startup.dispose.splice(0)) dispose()
    pluginV2.editoutput.clear()
    pluginV2.chatOutput.clear()
    boundary.record = null
    vi.unstubAllGlobals()
    vi.restoreAllMocks()
})

it('records applyGenerationResponse for synthetic replies through real sendChat', async () => {
    expect(new Set(ollamaCorpus.cases.map(entry => entry.id)).size).toBe(ollamaCorpus.cases.length)
    expect(ollamaCorpus.cases.every(entry => ['provider-envelope', 'owner-output'].includes(entry.oracleScope))).toBe(true)
    expect(providerEnvelope.length + ownerOutput.length).toBe(ollamaCorpus.cases.length)
    expect(providerEnvelope.map(entry => entry.id)).toEqual(providerEnvelopeIds)
    expect(ownerOutput.map(entry => entry.id)).toEqual(ownerOutputIds)
    vi.spyOn(console, 'log').mockImplementation(() => {})
    vi.spyOn(console, 'debug').mockImplementation(() => {})
    vi.spyOn(console, 'error').mockImplementation(() => {})
    vi.spyOn(console, 'warn').mockImplementation(() => {})
    vi.spyOn(Date, 'now').mockReturnValue(now)
    // One stub for the whole run: unstubbing would also drop the setup file's global stubs.
    let fetchHandler: ((url: string) => Response) | null = null
    let requests = 0
    vi.stubGlobal('fetch', vi.fn(async (input: RequestInfo | URL) => {
        const url = typeof input === 'string' ? input : input instanceof URL ? input.href : input.url
        if (!fetchHandler) throw new Error(`Unexpected request to ${url}`)
        requests++
        return fetchHandler(url)
    }))

    async function run(id: string, entry: Partial<Case>, messages: unknown[], reply: Case['reply']) {
        ids.next = 0
        resetScriptCache()
        CharEmotion.set({})
        const runtime = await activate(prepared(databaseFor(entry, messages)), `response-${id}`)
        const hooks: HookCall[] = []
        const replies = entry.replies ?? []
        for (let hook = 0; hook < (entry.hooks ?? 0); hook++) {
            pluginV2.editoutput.add(async (data: string) => {
                const spec = replies[hooks.length]
                let value: string | null | undefined
                if (!spec || 'undefined' in spec) value = undefined
                else if ('throw' in spec) { hooks.push({ hook, data, reply: { throw: spec.throw } }); throw new Error(spec.throw) }
                else if ('null' in spec) value = null
                else if ('suffix' in spec) value = data + spec.suffix
                else value = spec.value
                hooks.push({ hook, data, reply: value === undefined ? { undefined: true } : value === null ? { null: true } : { value } })
                return value
            })
        }
        let listenerCalls = 0
        for (let listener = 0; listener < (entry.listeners ?? 0); listener++) pluginV2.chatOutput.add(() => { listenerCalls++ })
        requests = 0
        fetchHandler = (url) => {
            if (requests !== 1) throw new Error(`${id}: repeated provider request`)
            if ('multiline' in reply) {
                expect(url).toBe('https://api.openai.com/v1/chat/completions')
                return openAIReply(reply.multiline)
            }
            expect(url).toBe('http://synthetic-ollama.invalid:80/api/chat')
            return ollamaReply(reply.content, reply.thinking)
        }
        alerts.errors.length = 0
        const record: { entry?: unknown; exit?: unknown } = {}
        boundary.record = record
        // Each case owns its reservation, as the chat screen does, so a thrown send cannot leave the next case refused.
        const reservation = reserveGeneration()
        if (!reservation) throw new Error(`${id}: generation already reserved`)
        let send: { returned: boolean } | { error: string }
        try {
            send = { returned: await sendChat({ historyLimit: false, continue: entry.continue === true }, reservation) }
        } catch (error) {
            send = { error: error instanceof Error ? error.message : String(error) }
        } finally {
            reservation.release()
        }
        expect(isGenerationReserved(), id).toBe(false)
        boundary.record = null
        fetchHandler = null
        await runtime.flushPendingDataLocally(`response-${id}-after`).catch(() => {})
        const emotions = get(CharEmotion)['character'] ?? null
        pluginV2.editoutput.clear()
        pluginV2.chatOutput.clear()
        return json({
            entry: record.entry ?? null, exit: record.exit ?? null, hooks, listenerCalls, emotions: emotions ?? null, send,
            alerts: [...alerts.errors], requests,
        })
    }

    const applications: unknown[] = []
    for (const entry of applicationSet.cases) {
        const reply = entry.reply
        const multiline = 'multiline' in reply
        const settings: Partial<Case> = multiline
            ? { ...entry, root: { genTime: 2, usePlainFetch: true, ...entry.root }, preset: { aiModel: 'gpt-5.4', ...entry.preset } }
            : entry
        applications.push(await run(entry.id, settings, entry.messages ?? base.messages, reply))
    }
    expect(settleOracle(applicationSet, applications, { generator: import.meta.url, tsSources, write: writeOllamaOnly ? false : undefined })).toMatch(/written|matched/)

    const ollama: unknown[] = []
    for (const entry of providerEnvelope) {
        const messages = entry.continue
            ? [...base.messages, { role: 'char', data: ollamaCorpus.continuePrefix, chatId: 'continue-base', saying: 'character', time: 100 }]
            : base.messages
        ollama.push(await run(`ollama-${entry.id}`, { continue: entry.continue }, messages, { content: entry.content, thinking: entry.thinking }))
    }
    if (writeOllamaOnly) {
        const previous = JSON.parse(readFileSync(ollamaSet.expectedPath, 'utf8')) as { cases: { id: string; result: unknown }[] }
        expect(previous.cases.map(entry => entry.id)).toEqual(ollamaSet.cases.map(entry => entry.id))
        for (const [index, entry] of previous.cases.entries()) expect(ollama[index], `Unchanged response oracle ${entry.id}`).toEqual(entry.result)
    }
    expect(settleOracle(ollamaSet, ollama, { generator: import.meta.url, tsSources, write: writeOllamaOnly || undefined })).toMatch(/written|matched/)
}, 600000)
