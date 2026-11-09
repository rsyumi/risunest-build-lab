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
// Fresh message and record IDs come from the random source; a counter keeps the expected files stable.
const ids = vi.hoisted(() => ({ next: 0 }))
vi.mock('uuid', async (original) => {
    const actual = await original<typeof import('uuid')>()
    return { ...actual, v4: () => `00000000-0000-4000-8000-${String(++ids.next).padStart(12, '0')}` }
})
// The alert dialog is the UI boundary where a failed preparation reports its error text.
const alerts = vi.hoisted(() => ({ errors: [] as string[] }))
vi.mock('../../src/ts/alert', async (original) => {
    const actual = await original<typeof import('../../src/ts/alert')>()
    return { ...actual, alertError: (message: string | Error) => { alerts.errors.push(String(message)) } }
})
vi.mock('../../src/ts/process/luaRuntime', async (original) => (await import('../generation-send/happyDomLuaRuntime')).luaRuntimeWithNodeLoader(original))
// The biases the preparation hands to the request; most providers drop or tokenize them before anything observable.
// The actual module is loaded at call time: loading it in the factory would bind modules on its import cycle to the
// unmocked module.
const requests = vi.hoisted(() => ({ biases: null as unknown }))
vi.mock(import('../../src/ts/process/request/request'), () => {
    type Request = typeof import('../../src/ts/process/request/request')
    const actual = () => vi.importActual<Request>('../../src/ts/process/request/request')
    return {
        requestChatData: async (...args: Parameters<Request['requestChatData']>) => {
            requests.biases = JSON.parse(JSON.stringify(args[0].biasString ?? null))
            return (await actual()).requestChatData(...args)
        },
        requestChatDataMain: async (...args: Parameters<Request['requestChatDataMain']>) => (await actual()).requestChatDataMain(...args),
    } satisfies Partial<Request>
})

import { deriveEffectivePresetMirrors, normalizeDatabaseDefaults, setDatabase, getDatabase, presetMirrorMap, type Database } from '../../src/ts/storage/database.svelte'
import { deriveEffectivePersonaMirrors, personaMirrorMap } from '../../src/ts/storage/effectiveIdentityState'
import { selectedCharID } from '../../src/ts/stores.svelte'
import { characterFormatUpdate } from '../../src/ts/characters'
import { getPersistentDataStore } from '../../src/ts/storage/persistentDataStoreFactory'
import { getPersistentDataRuntime } from '../../src/ts/storage/persistentDataRuntime.svelte'
import { createCatalogPresetWorkingSet, projectCatalogWorkingSet, projectCompleteScalableWorkingSet } from '../../src/ts/storage/workingSetCatalog'
import { bootstrapPersistentDatabase } from '../../src/ts/storage/persistentBootstrap'
import { prepareDatabaseForBootstrap, preparePersistentRootForWorkingSet } from '../../src/ts/storage/databasePreparation'
import { pluginV2 } from '../../src/ts/plugins/plugins.svelte'
import { sendChat, type OpenAIChat } from '../../src/ts/process/index.svelte'
import { updateStartupExclusions } from '../../src/ts/storage/deviceSettings'
import type { RecoveryExclusion } from '../../src/ts/storage/startupExclusions'
import { captureNativeHost } from '../../src/ts/process/nativeHostCapture'
import { luaJsonModuleResponse } from '../generation-send/happyDomLuaRuntime'
import { language } from '../../src/lang'

type RecordValue = Record<string, unknown>
type Hook = { suffix?: string; replace?: [string, string]; keep?: boolean; nullish?: boolean; empty?: boolean }
type SendCase = {
    id: string
    lorePlacement?: boolean
    root?: RecordValue; preset?: RecordValue; character?: RecordValue; conversation?: RecordValue; messages?: unknown[]
    // 'plugin' registers a custom plugin tokenizer; otherwise the provider's built-in tokenizer option.
    tokenizer?: 'plugin' | 'cl100k_base' | 'o200k_base' | 'none'
    hooks?: Hook[]
    // 'replacer' observes the final array through a pass-through request replacer, for models without the plugin provider.
    capture?: 'provider' | 'replacer'
    // The input the Rust machine declines; TS runs the case as usual.
    decline?: string
    // `Date.now()` and the `Math.random()` draws during the send.
    now?: number
    random?: number[]
    // The local time zone of `Date` and `Intl` during the send, whose `new Date()` then reads `now` too; the result records
    // the host capture the native route reserves with.
    timeZone?: string
    // What this start excludes, as a recovery start or the stored exclusions set it.
    startupExclusions?: RecoveryExclusion[]
}
// `edit` changes root fields of the active database after activation and saves them as an in-session edit does;
// the whole-database write would store an unresolved persona selection as an empty ID instead.
// `stored.removed` drops root fields from the database before its whole-database write, so the stored root lacks them.
type SettingsCase = { id: string; root?: RecordValue; preset?: RecordValue; stored?: { removed: string[] }; edit?: { set?: RecordValue; removed?: string[] } }
type Base = { root: RecordValue; preset: RecordValue; character: RecordValue; conversation: RecordValue; messages: unknown[] }
type HostEntry = { kind: 'tokenizer'; text: string; count: number } | { kind: 'editprocess'; input: string; output: string | null }

const sendSet = readOracleCases<SendCase>('prompt', 'send')
const sendBase = (JSON.parse(readFileSync(sendSet.casesPath, 'utf8')) as { base: Base }).base
const settingsSet = readOracleCases<SettingsCase>('prompt', 'settings')
// Settings cases carry only root and preset; the send fixture supplies the character.
const settingsBase: Base = { ...sendBase, ...(JSON.parse(readFileSync(settingsSet.casesPath, 'utf8')) as { base: Pick<Base, 'root' | 'preset'> }).base }

// The Ollama literal cases are read in place; its checkout may carry CRLF, so the hash covers LF-normalized text.
const ollamaPath = resolve('tests/fixtures/native-generation/ollama-text.json')
const ollamaSource = readFileSync(ollamaPath, 'utf8').replaceAll('\r\n', '\n')
const writeOllamaOnly = process.env.RISUNEST_OLLAMA_PROMPT_ORACLE_WRITE === '1'
if (writeOllamaOnly && process.env.RUST_CORE_ORACLE_WRITE === '1') throw new Error('Ollama-only capture requires RUST_CORE_ORACLE_WRITE unset')
const ollamaFixtures = JSON.parse(ollamaSource) as Base & { cases: (SendCase & { expectedIdAssignments?: number[]; triggerEditsMessages?: boolean })[] }
const ollamaSet: OracleSet<SendCase> = {
    root: sendSet.root,
    casesPath: ollamaPath,
    expectedPath: resolve(sendSet.root, 'tests/fixtures/rust-core/prompt/ollama-text.expected.json'),
    fixtureSha256: createHash('sha256').update(ollamaSource).digest('hex'),
    schema: 1,
    cases: ollamaFixtures.cases,
}

const tsSources = [
    'src/ts/process/index.svelte.ts', 'src/ts/process/promptHistory.ts', 'src/ts/process/currentChatParserPass.ts',
    'src/ts/process/exampleMessages.ts', 'src/ts/process/scripts.ts', 'src/ts/process/moduleSelection.ts', 'src/ts/process/triggers.ts',
    'src/ts/parser/chatML.ts', 'src/ts/parser/parser.svelte.ts', 'src/ts/cbs.ts', 'src/ts/util.ts', 'src/ts/tokenizer.ts',
    'src/ts/model/modellist.ts', 'src/ts/process/request/request.ts', 'src/ts/storage/database.svelte.ts',
    'src/ts/storage/effectiveIdentityState.ts', 'src/ts/process/nativeHostCapture.ts', 'src/ts/process/regexExecutionPlan.ts',
    'src/ts/process/lorebook.svelte.ts', 'src/ts/process/caseUnicode.ts',
    'src/ts/storage/deviceSettings.ts', 'src/ts/storage/recoveryMode.svelte.ts',
]
const settingsSources = [...tsSources, 'src/ts/storage/persistentBootstrap.ts', 'src/ts/storage/databasePreparation.ts',
    'src/ts/storage/workingSetCatalog.ts']

function databaseFor(base: Base, entry: SendCase): Database {
    const conversation = { ...base.conversation, ...entry.conversation, message: entry.messages ?? base.messages }
    const character = { ...base.character, ...entry.character, chats: [conversation] }
    return structuredClone({ ...base.root, ...entry.root, botPresets: [{ ...base.preset, ...entry.preset }], characters: [character] }) as unknown as Database
}

const json = <T>(value: T): T => JSON.parse(JSON.stringify(value)) as T

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
    return { store, runtime }
}

// A native route reads only stored records, so TS is observed after loading them: the store reads the stored
// selections back, and activation installs the loaded database.
async function reload(label: string) {
    return activate(await getPersistentDataStore().materializeDatabase(), label)
}

// The startup load `loadData` runs on the stored records, which also canonicalizes the stored preset selection.
async function startupLoad() {
    const { database } = await bootstrapPersistentDatabase({
        store: getPersistentDataStore(),
        prepareDatabase: prepareDatabaseForBootstrap,
        prepareRoot: preparePersistentRootForWorkingSet,
        projectScalableWorkingSet: (input) => projectCatalogWorkingSet(input.root, input.characters,
            createCatalogPresetWorkingSet(input.presetCatalog, input.activePreset)),
    })
    setDatabase(database)
    await getPersistentDataRuntime().initializeActiveWorkingSet(getDatabase())
}

async function nativeView() {
    const store = getPersistentDataStore()
    return {
        root: (await store.readRoot()).value,
        preset: (await store.readPreset('preset-a'))!.value,
        character: (await store.readCharacter('character'))!.value,
        conversation: (await store.readConversation('character', 'conversation'))!.value,
    }
}

function prepared(database: Database) {
    const result = normalizeDatabaseDefaults(database)
    characterFormatUpdate(result.characters[0])
    // characterFormatUpdate stamps these from the clock.
    result.characters[0].creation_date = 1767225600000
    result.characters[0].lastInteraction = 1767225600000
    deriveEffectivePresetMirrors(result)
    deriveEffectivePersonaMirrors(result)
    return result
}

function hookFunction(hook: Hook, host: HostEntry[], recording: () => boolean) {
    return (content: string) => {
        const output = hook.keep ? undefined : hook.nullish ? null : hook.empty ? ''
            : hook.replace ? content.replaceAll(hook.replace[0], hook.replace[1]) : content + (hook.suffix ?? '')
        if (recording()) host.push({ kind: 'editprocess', input: content, output: output ?? null })
        return output
    }
}

// Every case stores a full root of mostly identical defaults; later cases keep only their differences from the
// first case's root, and the replay applies them in place.
function compactRoots(results: { database: { root?: RecordValue; rootChanges?: unknown } }[]) {
    const base = results[0]?.database.root
    if (!base) return results
    for (const result of results.slice(1)) {
        const root = result.database.root!
        const set = Object.fromEntries(Object.entries(root).filter(([key, value]) =>
            !Object.hasOwn(base, key) || JSON.stringify(value) !== JSON.stringify(base[key])))
        const removed = Object.keys(base).filter(key => !Object.hasOwn(root, key))
        delete result.database.root
        result.database.rootChanges = { set, removed }
    }
    return results
}

// The clock and random CBS (`{{unixtime}}`, `{{random}}`, ...) read Date.now and Math.random; a case that pins them hands the
// native route the same values. The tape throws once spent, so a draw outside the parse shows up as a failed send. A case
// with a time zone also pins `new Date()`, which the local time CBS reads, and the zone of `Date` and `Intl`.
function pinEnvironment(entry: { id: string; now?: number; random?: number[]; timeZone?: string }) {
    const spies: { mockRestore(): void }[] = []
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
            return value
        }))
    }
    return () => { for (const spy of spies) spy.mockRestore() }
}

afterEach(() => {
    for (const dispose of startup.dispose.splice(0)) dispose()
    pluginV2.providers.clear()
    pluginV2.providerOptions.clear()
    pluginV2.editprocess.clear()
    pluginV2.replacerbeforeRequest.clear()
    vi.unstubAllGlobals()
    vi.restoreAllMocks()
})

async function encodeRecorder() {
    const { Tiktoken } = await import('@dqbd/tiktoken')
    const encode = Tiktoken.prototype.encode
    const counts = new Map<string, number>()
    let recording = false
    vi.spyOn(Tiktoken.prototype, 'encode').mockImplementation(function(this: InstanceType<typeof Tiktoken>, ...args: Parameters<typeof encode>) {
        const result = encode.apply(this, args)
        if (recording) {
            const previous = counts.get(args[0])
            if (previous !== undefined && previous !== result.length) throw new Error(`Inconsistent count for ${args[0]}`)
            counts.set(args[0], result.length)
        }
        return result
    })
    return {
        start() { counts.clear(); recording = true },
        stop() { recording = false; return [...counts.entries()] },
    }
}

it('captures prompt preparation from real sendChat for the Rust prompt machine', async () => {
    vi.spyOn(console, 'log').mockImplementation(() => {})
    vi.spyOn(console, 'debug').mockImplementation(() => {})
    vi.spyOn(console, 'error').mockImplementation(() => {})
    vi.spyOn(console, 'warn').mockImplementation(() => {})
    const encoder = await encodeRecorder()
    // One stub for the whole run: unstubbing would also drop the setup file's global stubs.
    let fetchHandler: ((input: RequestInfo | URL, init?: RequestInit) => Promise<Response>) | null = null
    vi.stubGlobal('fetch', vi.fn(async (input: RequestInfo | URL, init?: RequestInit) => {
        const luaJson = luaJsonModuleResponse(input)
        if (luaJson) return luaJson
        if (!fetchHandler) throw new Error(`Unexpected request to ${String(input)}`)
        return fetchHandler(input, init)
    }))

    const settings: unknown[] = []
    const mirrorKeys = [...Object.keys(presetMirrorMap), ...Object.keys(personaMirrorMap)]
    for (const entry of settingsSet.cases) {
        const database = prepared(databaseFor(settingsBase, entry))
        for (const key of entry.stored?.removed ?? []) delete (database as unknown as RecordValue)[key]
        const { runtime } = await activate(database, `prompt-settings-${entry.id}`)
        if (entry.edit) {
            const edited = getDatabase() as unknown as RecordValue
            Object.assign(edited, structuredClone(entry.edit.set ?? {}))
            for (const key of entry.edit.removed ?? []) delete edited[key]
            await runtime.flushPendingDataLocally(`prompt-settings-${entry.id}-edit`)
            await immediate()
        }
        const view = await nativeView()
        await startupLoad()
        const active = getDatabase() as unknown as RecordValue
        // JSON has no undefined; an absent or undefined mirror is recorded as such.
        const effective = Object.fromEntries(mirrorKeys.map(key => [key, active[key] === undefined
            ? { undefined: true } : { value: active[key] }]))
        settings.push(json({ database: { root: view.root, preset: view.preset }, effective }))
    }
    expect(settleOracle(settingsSet, compactRoots(settings as never), { generator: import.meta.url, tsSources: settingsSources, write: false })).toMatch(/written|matched/)

    const results: unknown[] = []
    for (const entry of sendSet.cases) {
        const database = prepared(databaseFor(sendBase, entry))
        await activate(database, `prompt-send-${entry.id}`)
        const view = await nativeView()
        const { store, runtime } = await reload(`prompt-send-${entry.id}-reload`)
        const host: HostEntry[] = []
        let recording = false
        const provider = String(getDatabase().currentPluginProvider || 'synthetic-provider')
        const kind = entry.tokenizer ?? 'plugin'
        pluginV2.providerOptions.set(provider, kind === 'plugin'
            ? { tokenizer: 'custom', tokenizerFunc: (text: string) => {
                const count = Math.ceil(text.length / 4)
                if (recording) host.push({ kind: 'tokenizer', text, count })
                return new Array<number>(count).fill(0)
            } }
            : kind === 'none' ? {} : { tokenizer: kind })
        for (const hook of entry.hooks ?? []) pluginV2.editprocess.add(hookFunction(hook, host, () => recording))
        let request: { prompt_chat: OpenAIChat[]; max_tokens: number | null } | null = null
        const capture = (formated: OpenAIChat[], maxTokens: number | null) => {
            if (request) throw new Error(`Unexpected repeated provider request in ${entry.id}`)
            recording = false
            request = json({ prompt_chat: formated, max_tokens: maxTokens })
        }
        if ((entry.capture ?? 'provider') === 'provider') {
            pluginV2.providers.set(provider, async (arg) => {
                capture(arg.prompt_chat as OpenAIChat[], arg.max_tokens as number)
                return { success: true, content: 'Synthetic response.' }
            })
        } else {
            // The replacer sees the array before the provider's own formatting; its token limit is not observable here.
            pluginV2.replacerbeforeRequest.add((formated) => { capture(formated, null); return formated })
            fetchHandler = async () => new Response(JSON.stringify({
                choices: [{ index: 0, message: { role: 'assistant', content: 'Synthetic response.' }, finish_reason: 'stop' }],
            }), { status: 200, headers: { 'Content-Type': 'application/json' } })
        }
        alerts.errors.length = 0
        requests.biases = null
        encoder.start()
        recording = true
        const restore = pinEnvironment(entry)
        const sent = await sendChat({ historyLimit: false }).catch((error: unknown) => {
            throw new Error(`sendChat failed in ${entry.id}`, { cause: error })
        }).finally(restore)
        recording = false
        const tokens = encoder.stop()
        // A flush TS cannot complete is part of what the send leaves stored.
        const flushError = await runtime.flushPendingDataLocally(`prompt-send-${entry.id}-after`)
            .then(() => null, (error: unknown) => error instanceof Error ? error.message : String(error))
        const saved = (await store.readConversation('character', 'conversation'))!.value
        const info = sent ? saved.message.at(-1)?.generationInfo : undefined
        // What the send left stored, failed or not: the history it rewrote and the chat variables.
        const history = saved.message.slice(0, view.conversation.message.length).map(message => message.data)
        const rewritten = JSON.stringify(history) === JSON.stringify(view.conversation.message.map(message => message.data)) ? null : history
        results.push(json({
            database: view, sent, request, biases: requests.biases,
            generation: info ? { inputTokens: info.inputTokens, outputTokens: info.outputTokens, maxContext: info.maxContext } : null,
            lastMemory: saved.lastMemory ?? null,
            scriptstate: saved.scriptstate ?? null,
            rewritten, flushError,
            errors: [...alerts.errors],
            host, tokens,
        }))
        pluginV2.providers.clear()
        pluginV2.providerOptions.clear()
        pluginV2.editprocess.clear()
        pluginV2.replacerbeforeRequest.clear()
        fetchHandler = null
    }
    const compact = compactRoots(results as never)
    const previous = JSON.parse(readFileSync(sendSet.expectedPath, 'utf8')) as { cases: { id: string; result: unknown }[] }
    for (const [index, entry] of sendSet.cases.entries()) {
        if (!entry.lorePlacement) {
            expect(compact[index], `Unchanged no-lore oracle ${entry.id}`).toEqual(previous.cases.find(value => value.id === entry.id)!.result)
        }
    }
    expect(settleOracle(sendSet, compact, { generator: import.meta.url, tsSources, write: writeOllamaOnly ? false : undefined })).toMatch(/written|matched/)

    // The Ollama IDs do not depend on how many send cases ran before them.
    ids.next = 0
    const ollama: unknown[] = []
    for (const entry of ollamaFixtures.cases) {
        const database = prepared(databaseFor(ollamaFixtures, entry))
        await activate(database, `prompt-ollama-${entry.id}`)
        const view = await nativeView()
        const { store, runtime } = await reload(`prompt-ollama-${entry.id}-reload`)
        let body: { messages: { role: string; content: string }[] } | null = null
        fetchHandler = async (input, init) => {
            expect(String(input)).toBe('http://synthetic-ollama.invalid:80/api/chat')
            if (body) throw new Error(`Unexpected repeated provider request in ${entry.id}`)
            body = JSON.parse(String(init?.body)) as typeof body
            return new Response(JSON.stringify({ model: 'synthetic-model', done: true, message: { role: 'assistant', content: 'Synthetic response.' } }), {
                status: 200, headers: { 'Content-Type': 'application/json' },
            })
        }
        alerts.errors.length = 0
        encoder.start()
        const restore = pinEnvironment(entry)
        updateStartupExclusions(entry.startupExclusions ?? [])
        const host = entry.timeZone === undefined ? undefined : captureNativeHost()
        const sent = await sendChat({ historyLimit: false })
        updateStartupExclusions([])
        restore()
        const tokens = encoder.stop()
        await runtime.flushPendingDataLocally(`prompt-ollama-${entry.id}-after`)
        const saved = (await store.readConversation('character', 'conversation'))!.value
        // The send gives nullish message IDs fresh values before preparation; the native owner does the same first. A start
        // trigger that edits the stored history has no such IDs to assign, and the history it read stays the input.
        if (entry.triggerEditsMessages) expect(view.conversation.message.every(message => message.chatId != null), entry.id).toBe(true)
        else view.conversation = { ...view.conversation, message: saved.message.slice(0, view.conversation.message.length) }
        const info = sent ? saved.message.at(-1)?.generationInfo : undefined
        ollama.push(json({
            database: view, sent, messages: body ? body!.messages : null,
            generation: info ? { inputTokens: info.inputTokens, outputTokens: info.outputTokens, maxContext: info.maxContext } : null,
            lastMemory: saved.lastMemory ?? null,
            errors: [...alerts.errors],
            tokens,
            ...(host ? { host } : {}),
        }))
        fetchHandler = null
    }
    const compactOllama = compactRoots(ollama as never)
    if (writeOllamaOnly) {
        const previousOllama = JSON.parse(readFileSync(ollamaSet.expectedPath, 'utf8')) as { cases: { id: string; result: unknown }[] }
        for (const [index, previous] of previousOllama.cases.entries()) {
            expect(ollamaSet.cases[index]?.id, 'Existing Ollama case order').toBe(previous.id)
            expect(compactOllama[index], `Unchanged Ollama oracle ${previous.id}`).toEqual(previous.result)
        }
    }
    expect(settleOracle(ollamaSet, compactOllama, { generator: import.meta.url, tsSources, write: writeOllamaOnly })).toMatch(/written|matched/)
    expect(language.errors.toomuchtoken.length).toBeGreaterThan(0)
}, 600000)
