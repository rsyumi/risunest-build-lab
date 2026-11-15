import { createHash } from 'node:crypto'
import { readFileSync } from 'node:fs'
import { dirname, join } from 'node:path'
import { fileURLToPath } from 'node:url'
import { afterEach, beforeAll, beforeEach, expect, it, vi } from 'vitest'

// Effect roots created while the stores load stay inert: the oracle reads settings through getDatabase only.
vi.mock('svelte/internal/client', async (original) => {
    const actual = await original<typeof import('svelte/internal/client')>()
    return { ...actual, effect_root: () => () => {} }
})
// Node loads the package's UMD bundle as an ES module, which publishes the real exports on globalThis.tokenizers
// instead of as named exports; the app Vitest project bundles it through the optimizer for the same reason.
vi.mock('@mlc-ai/web-tokenizers', async (original) => {
    await original()
    return { ...(globalThis as { tokenizers?: object }).tokenizers }
})

import { getDatabase, normalizeDatabaseDefaults, setDatabase, type character, type Chat, type Database } from '../../src/ts/storage/database.svelte'
import { getModelInfo } from '../../src/ts/model/modellist'
import { supportsInlayImage } from '../../src/ts/process/files/inlays'
import { risuChatParser } from '../../src/ts/parser/parser.svelte'
import { pluginV2 } from '../../src/ts/plugins/plugins.svelte'
import type { OpenAIChat } from '../../src/ts/process/index.svelte'
import {
    ChatTokenizer, encode, encodeWithTokenizer, getCharToken, getChatToken, strongBan, tokenize, tokenizeAccurate, tokenizeNum,
} from '../../src/ts/tokenizer'
import { readOracleCases, settleOracle, type OracleCase } from './oracleFile'

// Strings that are not well-formed travel as their UTF-16 units, since serde_json rejects isolated surrogate escapes.
type Text = string | { u: number[] }
type Reply = 'code-points' | 'half-length' | 'none' | 'empty'
type Fetched = { url: string; method?: string; headers?: [string, string][]; body?: string; sha256?: string }

// The file system serves admitted SentencePiece and HF models from public/token. Other Hugging Face artifacts stay
// withheld unless a set serves them, so those tokenizers stop at their fetch and name the artifact they would load.
const artifactBoundary = 'synthetic artifact boundary'
const repositoryRoot = join(dirname(fileURLToPath(import.meta.url)), '../..')
const generator = import.meta.url
const tokenizerSource = 'src/ts/tokenizer.ts'

function decodeText(text: Text) {
    if (typeof text === 'string') return text
    let value = ''
    for (const unit of text.u) value += String.fromCharCode(unit)
    return value
}
// String.prototype.isWellFormed is ES2024, past the configured lib.
function wellFormed(text: string) {
    for (let index = 0; index < text.length; index++) {
        const unit = text.charCodeAt(index)
        if (unit >= 0xdc00 && unit <= 0xdfff) return false
        if (unit >= 0xd800 && unit <= 0xdbff) {
            const next = text.charCodeAt(index + 1)
            if (!(next >= 0xdc00 && next <= 0xdfff)) return false
            index++
        }
    }
    return true
}
function encodeText(text: string): Text {
    return wellFormed(text) ? text : { u: Array.from({ length: text.length }, (_, index) => text.charCodeAt(index)) }
}
const message = (error: unknown) => error instanceof Error ? error.message : String(error)

// An artifact's fingerprint: SHA-256 of its bytes, reading CRLF as LF in the JSON artifacts, which Git checks out as text.
// A raw CR cannot occur inside a JSON string, so the normalization touches only whitespace.
function fingerprint(file: string, bytes: Buffer) {
    const normalized = file.endsWith('.json') ? Buffer.from(bytes.toString('latin1').replaceAll('\r\n', '\n'), 'latin1') : bytes
    return { sha256: createHash('sha256').update(normalized).digest('hex'), bytes: normalized.length }
}

// The synthetic plugin tokenizers, by the reply rule a case names.
const replies: Record<Reply, (text: string) => number[] | undefined> = {
    'code-points': (text) => Array.from(text, (character) => character.codePointAt(0)!),
    'half-length': (text) => [text.length / 2],
    none: () => undefined,
    empty: () => [],
}

let fetches: Fetched[] = []
let requests: Text[] = []
let google: { status: number; totalTokens?: number } | undefined
// The countTokens answers of a google-cloud case, taken in order.
let googleAnswers: GoogleAnswer[] | undefined
let provider = ''
// Whether the Hugging Face artifacts are served too.
let servesJson = false

// Starts the tokenizer WASM on its WebView path, as the app project's optimizer define does. On its Node path it resolves
// its own location through document.baseURI, which happy-dom answers with an http URL that createRequire rejects.
beforeAll(async () => {
    const { Tokenizer } = await import('@mlc-ai/web-tokenizers')
    const node = Object.getOwnPropertyDescriptor(process.versions, 'node')!
    Object.defineProperty(process.versions, 'node', { ...node, value: undefined })
    try {
        const model = readFileSync(join(repositoryRoot, 'public/token/mistral/tokenizer.model'))
        const tokenizer = await Tokenizer.fromSentencePiece(model.buffer.slice(model.byteOffset, model.byteOffset + model.byteLength) as ArrayBuffer)
        tokenizer.dispose()
    } finally {
        Object.defineProperty(process.versions, 'node', node)
    }
})

beforeEach(() => {
    fetches = []
    servesJson = false
    requests = []
    google = undefined
    googleAnswers = undefined
    // A spy rather than vi.stubGlobal: unstubbing would also drop the globals vitest.setup.ts stubs.
    vi.spyOn(globalThis, 'fetch').mockImplementation(async (input: RequestInfo | URL, init?: RequestInit) => {
        const url = String(input)
        const record: Fetched = { url }
        if (init?.method) record.method = init.method
        if (init?.headers) record.headers = Object.entries(init.headers as Record<string, string>)
        if (typeof init?.body === 'string') record.body = init.body
        fetches.push(record)
        if (url.startsWith('/token/')) {
            if (url.endsWith('.json') && !['/token/claude/claude.json', '/token/deepseek/tokenizer.json', '/token/deepseek/v4/tokenizer.json', '/token/llama/llama3.json', '/token/glm4/tokenizer.json', '/token/glm5/tokenizer.json', '/token/gemma/gemma4.json', '/token/cohere/tokenizer.json'].includes(url) && !servesJson) throw new TypeError(artifactBoundary)
            const bytes = readFileSync(join(repositoryRoot, 'public', url))
            record.sha256 = fingerprint(url, bytes).sha256
            return new Response(new Uint8Array(bytes), { status: 200 })
        }
        const answer = url.startsWith('https://generativelanguage.googleapis.com/') ? googleAnswers?.shift() : undefined
        if (answer) {
            if ('throws' in answer) throw new TypeError(answer.throws)
            return new Response('bytes' in answer ? new Uint8Array(answer.bytes) : answer.body, { status: answer.status })
        }
        if (url.startsWith('https://generativelanguage.googleapis.com/') && google) {
            const body = google.totalTokens === undefined ? {} : { totalTokens: google.totalTokens }
            return new Response(JSON.stringify(body), { status: google.status, headers: { 'Content-Type': 'application/json' } })
        }
        throw new Error(`Unexpected fetch ${url}`)
    })
})
afterEach(() => {
    pluginV2.providerOptions.delete(provider)
    vi.restoreAllMocks()
})

type Settings = {
    aiModel: string
    customTokenizer: unknown
    googleClaudeTokenizing?: boolean
    model?: { tokenizer?: unknown }
    plugin?: { tokenizer?: unknown; reply?: Reply }
}
type Extra = { caching?: boolean; inlay?: boolean; visionQuality?: string; customFlags?: boolean }

// Sets the database and plugin provider a case describes. Every case gets its own provider name, which the encode
// cache key includes, so plugin requests start from an empty cache.
function configure(settings: Settings, extra: Extra, name: string) {
    provider = `oracle-${name}`
    const flags = extra.inlay ? [0] : []
    const customModels = settings.model ? [{
        id: settings.aiModel, internalId: `synthetic-${name}`, url: '', format: 0, key: '', name: 'Synthetic', params: '', flags,
        ...('tokenizer' in settings.model ? { tokenizer: settings.model.tokenizer } : {}),
    }] : []
    setDatabase(normalizeDatabaseDefaults({
        aiModel: settings.aiModel,
        customTokenizer: settings.customTokenizer,
        googleClaudeTokenizing: settings.googleClaudeTokenizing ?? false,
        useTokenizerCaching: extra.caching ?? false,
        currentPluginProvider: provider,
        customModels,
        google: { accessToken: 'synthetic-google-key' },
        gptVisionQuality: extra.visionQuality ?? 'low',
        enableCustomFlags: extra.customFlags ?? false,
        customFlags: flags,
    } as unknown as Database))
    if (settings.plugin) {
        const options: { tokenizer?: unknown; tokenizerFunc?: (text: string) => Promise<number[] | undefined> } = {}
        if ('tokenizer' in settings.plugin) options.tokenizer = settings.plugin.tokenizer
        const reply = settings.plugin.reply
        if (reply) options.tokenizerFunc = async (text) => { requests.push(encodeText(text)); return replies[reply](text) }
        pluginV2.providerOptions.set(provider, options as never)
    }
}

const modelTokenizer = (value: unknown) => typeof value === 'number' ? value : { type: typeof value }

type SelectionCase = OracleCase & { text: Text; settings: Settings; google?: { status: number; totalTokens?: number } }

it('records which tokenizer encode runs for each setting', async () => {
    const set = readOracleCases<SelectionCase>('tokenizer', 'selection')
    const results = []
    for (const entry of set.cases) {
        configure(entry.settings, {}, `selection-${entry.id}`)
        google = entry.google
        const model = getModelInfo(entry.settings.aiModel)
        // getModelInfo runs here, so the replay takes the tokenizer and internalID it found.
        const result: Record<string, unknown> = { modelTokenizer: modelTokenizer(model.tokenizer), internalID: model.internalID }
        try {
            result.ids = Array.from(await encode(decodeText(entry.text)))
        } catch (error) {
            result.error = message(error)
        }
        result.fetches = fetches
        result.requests = requests
        results.push(result)
        afterEachCase()
    }
    expect(settleOracle(set, results, { generator, tsSources: [tokenizerSource, 'src/ts/tokenizer/gemmaTokenizer.ts', 'src/ts/model/modellist.ts'] }))
        .toMatch(/^(written|matched)$/)
}, 120_000)

function afterEachCase() {
    pluginV2.providerOptions.delete(provider)
    fetches = []
    requests = []
    google = undefined
}

type AccountingCase = OracleCase & Extra & {
    call: 'tokenize' | 'tokenizeNum' | 'tokenizeChat' | 'tokenizeChats' | 'getChatToken' | 'getCharToken' | 'tokenizeAccurate'
    tokenizer: 'cl100k_base' | 'o200k_base' | 'claude' | 'deepseek' | 'deepseek-v4' | 'llama3' | 'glm4' | 'glm5' | 'gemma' | 'cohere' | 'plugin' | 'plugin_missing'
    reply?: Reply
    text?: Text
    additional?: number
    useName?: 'name' | 'noName'
    message?: OpenAIChat
    messages?: OpenAIChat[]
    chat?: Text[]
    character?: Record<string, unknown>
    consistantChar?: boolean
}

function accountingSettings(entry: AccountingCase): [Settings, Extra] {
    const extra: Extra = { caching: entry.caching, inlay: entry.inlay, visionQuality: entry.visionQuality }
    switch (entry.tokenizer) {
        case 'cl100k_base': return [{ aiModel: `xcustom:::accounting-${entry.id}`, customTokenizer: 'tik', model: { tokenizer: 1 } }, extra]
        case 'o200k_base': return [{ aiModel: `xcustom:::accounting-${entry.id}`, customTokenizer: 'tik', model: { tokenizer: 2 } }, extra]
        case 'claude': return [{ aiModel: `xcustom:::accounting-${entry.id}`, customTokenizer: 'tik', model: { tokenizer: 6 } }, extra]
        case 'deepseek': return [{ aiModel: `xcustom:::accounting-${entry.id}`, customTokenizer: 'tik', model: { tokenizer: 13 } }, extra]
        case 'deepseek-v4': return [{ aiModel: `xcustom:::accounting-${entry.id}`, customTokenizer: 'tik', model: { tokenizer: 14 } }, extra]
        case 'llama3': return [{ aiModel: 'custom', customTokenizer: 'tik', plugin: { tokenizer: 'llama3' } }, { ...extra, customFlags: true }]
        case 'glm4': return [{ aiModel: `xcustom:::accounting-${entry.id}`, customTokenizer: 'tik', model: { tokenizer: 15 } }, extra]
        case 'glm5': return [{ aiModel: `xcustom:::accounting-${entry.id}`, customTokenizer: 'tik', model: { tokenizer: 16 } }, extra]
        case 'gemma': return [{ aiModel: `xcustom:::accounting-${entry.id}`, customTokenizer: 'tik', model: { tokenizer: 9 } }, extra]
        case 'cohere': return [{ aiModel: `xcustom:::accounting-${entry.id}`, customTokenizer: 'tik', model: { tokenizer: 11 } }, extra]
        case 'plugin': return [{ aiModel: 'custom', customTokenizer: 'tik', plugin: { tokenizer: 'custom', reply: entry.reply } }, { ...extra, customFlags: true }]
        case 'plugin_missing': return [{ aiModel: 'custom', customTokenizer: 'tik', plugin: { tokenizer: 'custom' } }, { ...extra, customFlags: true }]
    }
}

async function account(entry: AccountingCase): Promise<unknown> {
    const tokenizer = () => new ChatTokenizer(entry.additional ?? 0, entry.useName ?? 'name')
    switch (entry.call) {
        case 'tokenize': return await tokenize(decodeText(entry.text!))
        case 'tokenizeNum': return Array.from(await tokenizeNum(decodeText(entry.text!)))
        case 'tokenizeChat': return await tokenizer().tokenizeChat(entry.message!)
        case 'tokenizeChats': return await tokenizer().tokenizeChats(entry.messages!)
        case 'getChatToken': return await getChatToken({
            message: entry.chat!.map((data, index) => ({ role: index % 2 ? 'char' : 'user', data: decodeText(data) })),
        } as unknown as Chat)
        case 'getCharToken': return await getCharToken(structuredClone(entry.character) as unknown as character)
        case 'tokenizeAccurate': {
            const data = decodeText(entry.text!)
            const value = await tokenizeAccurate(data, entry.consistantChar)
            // The text tokenizeAccurate hands risuChatParser and what the parser returns for it, a tape the Rust
            // replay stands in for the CBS processor.
            const input = data.replace('{{slot}}', '')
            parsed = { input: encodeText(input), output: encodeText(risuChatParser(input, { tokenizeAccurate: true, consistantChar: entry.consistantChar })) }
            return value
        }
    }
}
let parsed: { input: Text; output: Text } | undefined

it('records tokenize, ChatTokenizer and the counts built on them', async () => {
    const set = readOracleCases<AccountingCase>('tokenizer', 'accounting')
    const results = []
    for (const entry of set.cases) {
        const [settings, extra] = accountingSettings(entry)
        configure(settings, extra, `accounting-${entry.id}`)
        // The case's inlay setting reaches tokenizeMultiModal through the model flags.
        expect(supportsInlayImage(), entry.id).toBe(entry.inlay ?? false)
        const result: Record<string, unknown> = {}
        parsed = undefined
        try {
            result.value = await account(entry)
        } catch (error) {
            result.error = message(error)
        }
        if (parsed) result.parsed = parsed
        result.requests = requests
        results.push(result)
        afterEachCase()
    }
    expect(settleOracle(set, results, { generator, tsSources: [tokenizerSource, 'src/ts/process/files/inlays.ts', 'src/ts/parser/parser.svelte.ts'] })).toMatch(/^(written|matched)$/)
}, 120_000)

type BanCase = OracleCase & { tokenizer: AccountingCase['tokenizer']; reply?: Reply; caching?: boolean; data: Text; bias: string; cache?: string }

it('records strongBan expansions and its localStorage cache', async () => {
    // toLocaleUpperCase follows the default locale; the expectations hold for locales without case tailoring.
    expect(['i'.toLocaleUpperCase(), 'I'.toLocaleLowerCase(), 'i̇'.toLocaleUpperCase()]).toEqual(['I', 'i', 'İ'])
    const set = readOracleCases<BanCase>('tokenizer', 'strong-ban')
    const results = []
    for (const entry of set.cases) {
        const [settings, extra] = accountingSettings({ ...entry, id: entry.id, call: 'tokenize' })
        configure(settings, { ...extra, caching: entry.caching }, `strong-ban-${entry.id}`)
        const data = decodeText(entry.data)
        const key = 'strongBan_' + data
        localStorage.removeItem(key)
        if (entry.cache !== undefined) localStorage.setItem(key, entry.cache)
        const result: Record<string, unknown> = {}
        try {
            result.result = JSON.stringify(await strongBan(data, JSON.parse(entry.bias)))
        } catch (error) {
            result.error = message(error)
        }
        result.stored = localStorage.getItem(key)
        result.requests = requests
        localStorage.removeItem(key)
        results.push(result)
        afterEachCase()
    }
    expect(settleOracle(set, results, { generator, tsSources: [tokenizerSource] })).toMatch(/^(written|matched)$/)
}, 120_000)

type EncodeCase = OracleCase & { tokenizer: string; input: Text | { repeat: Text; count: number } }

// encodeWithTokenizer per case, with the fetches each call made.
async function encodeCases(cases: EncodeCase[]) {
    const results = []
    for (const entry of cases) {
        const input = typeof entry.input === 'object' && 'repeat' in entry.input
            ? decodeText(entry.input.repeat).repeat(entry.input.count) : decodeText(entry.input)
        fetches = []
        const result: Record<string, unknown> = {}
        try {
            result.ids = Array.from(await encodeWithTokenizer(input, entry.tokenizer))
        } catch (error) {
            result.error = message(error)
        }
        result.fetches = fetches
        results.push(result)
    }
    return results
}

it('records encodeWithTokenizer over the tiktoken corpus and every tokenizer name', async () => {
    const set = readOracleCases<EncodeCase>('tokenizer', 'encode')
    const results = await encodeCases(set.cases)
    expect(settleOracle(set, results, { generator, tsSources: [tokenizerSource, 'src/ts/tokenizer/gemmaTokenizer.ts'] })).toMatch(/^(written|matched)$/)
}, 120_000)

type GoogleAnswer = { status: number; body: string } | { status: number; bytes: number[] } | { throws: string }
type GoogleStep = { call: 'encode' | 'chat'; model: number; text: Text; name?: string; additional?: number; decline?: string }
type GoogleCase = OracleCase & { models: string[]; token: string | null; steps: GoogleStep[]; responses: GoogleAnswer[] }

// tokenizeGoogleCloud through encode and ChatTokenizer. TS keeps its memo for the module's life, so each case's steps
// share it; the steps of a case run as one session, each with its own model.
it('records the Google Cloud countTokens requests, answers and memo', async () => {
    const set = readOracleCases<GoogleCase>('tokenizer', 'google-cloud')
    const results = []
    for (const entry of set.cases) {
        googleAnswers = [...entry.responses]
        const customModels = entry.models.map((internalId, index) => ({
            id: `xcustom:::google-cloud-${entry.id}-${index}`, internalId, url: '', format: 0, key: '', name: 'Synthetic', params: '',
            flags: [], tokenizer: 10,
        }))
        const steps = []
        for (const step of entry.steps) {
            setDatabase(normalizeDatabaseDefaults({
                aiModel: customModels[step.model].id, customTokenizer: 'tik', googleClaudeTokenizing: true, useTokenizerCaching: false,
                currentPluginProvider: `oracle-google-cloud-${entry.id}`, customModels,
                ...(entry.token === null ? {} : { google: { accessToken: entry.token } }),
            } as unknown as Database))
            if (entry.token === null) delete (getDatabase() as { google?: unknown }).google
            fetches = []
            const result: Record<string, unknown> = {}
            try {
                const text = decodeText(step.text)
                result.value = step.call === 'encode'
                    ? Array.from(await encode(text))
                    : await new ChatTokenizer(step.additional ?? 0, 'name').tokenizeChat({ role: 'user', content: text, name: step.name } as OpenAIChat)
            } catch (error) {
                result.error = message(error)
            }
            result.fetches = fetches.map((fetched) => ({ ...fetched, url: encodeText(fetched.url) }))
            steps.push(result)
        }
        results.push({ steps, unanswered: googleAnswers.length })
    }
    googleAnswers = undefined
    expect(settleOracle(set, results, { generator, tsSources: [tokenizerSource, 'src/ts/tokenizer/gemmaTokenizer.ts', 'src/ts/model/modellist.ts'] }))
        .toMatch(/^(written|matched)$/)
}, 120_000)

// Every web tokenizer over its real artifact. A tokenizer loads at its first case, whose fetches carry the artifact's
// fingerprint.
it('records every web tokenizer over its real artifact', async () => {
    const set = readOracleCases<EncodeCase>('tokenizer', 'web-encode')
    servesJson = true
    const results = await encodeCases(set.cases)
    const reference = JSON.parse(readFileSync(join(repositoryRoot, 'tests/fixtures/tokenizer/gemma4.json'), 'utf8')) as {
        cases: { name: string; text: string; ids: number[] }[]
    }
    expect(reference.cases).toHaveLength(8)
    for (const entry of reference.cases) {
        const index = set.cases.findIndex(item => item.tokenizer === 'gemma' && item.input === entry.text)
        expect(index, entry.name).toBeGreaterThanOrEqual(0)
        expect(results[index].ids, entry.name).toEqual(entry.ids)
    }
    expect(settleOracle(set, results, { generator, tsSources: [tokenizerSource, 'src/ts/tokenizer/gemmaTokenizer.ts'] })).toMatch(/^(written|matched)$/)
}, 900_000)

type ArtifactCase = OracleCase & { tokenizer: string; file: string }

// A contract: the fingerprints of the artifacts TS loads, as the repository stores them.
it('records the artifact bytes behind every tokenizer', () => {
    const set = readOracleCases<ArtifactCase>('tokenizer', 'artifacts')
    const results = set.cases.map((entry) => fingerprint(entry.file, readFileSync(join(set.root, entry.file))))
    expect(settleOracle(set, results, { generator, tsSources: [] })).toMatch(/^(written|matched)$/)
}, 30_000)
