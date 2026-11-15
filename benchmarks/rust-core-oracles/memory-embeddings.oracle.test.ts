import { afterAll, afterEach, expect, it, vi } from 'vitest'
import 'fake-indexeddb/auto'
import { readFileSync, writeFileSync } from 'node:fs'
import { dirname, join } from 'node:path'
import { readOracleCases, settleOracle } from './oracleFile'

const boundary = vi.hoisted(() => {
    const state = { handler: null as unknown, violations: [] as string[], previous: Object.getOwnPropertyDescriptor(window, '__TAURI_INTERNALS__') }
    const reject = (message: string): never => { state.violations.push(message); throw new Error(message) }
    Object.defineProperty(window, '__TAURI_INTERNALS__', { configurable: true, value: {
        metadata: { currentWindow: { label: 'synthetic' }, currentWebview: { label: 'synthetic' } },
        invoke: (command: string) => Promise.reject(new Error((state.violations.push(`Unexpected IPC: ${command}`), `Unexpected IPC: ${command}`))),
        transformCallback: () => reject('Unexpected native callback registration'),
    } })
    return state
})
vi.mock('svelte/internal/client', async (original) => ({
    ...(await original<typeof import('svelte/internal/client')>()), effect_root: () => () => {},
}))
vi.mock('@mlc-ai/web-tokenizers', async (original) => {
    await original()
    return { ...(globalThis as { tokenizers?: object }).tokenizers }
})
vi.mock('@tauri-apps/plugin-http', async (original) => ({
    ...(await original<typeof import('@tauri-apps/plugin-http')>()),
    fetch: (async (input, init) => (boundary.handler as Transport)('native', input, init)) satisfies typeof import('@tauri-apps/plugin-http')['fetch'],
}))

import * as globalApi from '../../src/ts/globalApi.svelte'
import { getDatabase, normalizeDatabaseDefaults, setDatabase, type Database } from '../../src/ts/storage/database.svelte'
import { HypaProcesser, type HypaModel } from '../../src/ts/process/memory/hypamemory'
import { hypaEmbeddingIdentity } from '../../src/ts/process/memory/hypaCacheKey'
import { isTauri } from '../../src/ts/platform'

type U16 = number[]
type OptionalText = { u: U16 } | { absent: true }
type Settings = { hypaModel?: HypaModel; customUrl?: OptionalText; customModel?: OptionalText; customKey?: OptionalText; supaKey?: OptionalText }
type Input = { shape: 'scalar'; text: U16 } | { shape: 'batch'; texts: U16[] }
type Reply = { kind: 'http'; status: number; bytes: number[] } | { kind: 'transport'; name: string; message: U16 }
type Call = { before?: Settings; oaikey?: OptionalText; purpose: 'document' | 'query'; input: Input; duringReply?: Settings; reply: Reply
    failureComparison?: 'exact-remote-json' | 'incidental-shape' | 'incidental-decode' | 'incidental-transport' | 'incidental-url'
    nativeComparison?: 'reject-unsupported-shape'; sourceBoundary?: 'before-request' | 'before-transport' | 'transport' }
type Entry = { id: string; input: { transport: 'plain' | 'native'; construct: { model: HypaModel | 'auto'; customUrl: OptionalText; settings: Settings }; calls: Call[] } }
type Transport = (kind: 'plain' | 'native', input: RequestInfo | URL, init?: { headers?: HeadersInit; method?: string; body?: unknown; dataText?: string }) => Promise<Response>
type Raw = { constructedUrl: U16; constructedUrlUsv: U16; headers: [U16, U16][]; bodyText: U16; abortSignal: 'absent' | 'present' }
const units = (value: string): U16 => Array.from({ length: value.length }, (_, index) => value.charCodeAt(index))
const text = (value: U16): string => value.map(unit => String.fromCharCode(unit)).join('')
const optional = (value: OptionalText | undefined): string | undefined => value && 'u' in value ? text(value.u) : undefined
const encodedOptional = (value: unknown): OptionalText => value === undefined ? { absent: true } : { u: units(String(value)) }
const usv = (value: string) => new TextDecoder('utf-8', { ignoreBOM: true }).decode(new TextEncoder().encode(value))
function pack(value: unknown): unknown {
    if (value === undefined) return { undefined: true }
    if (typeof value === 'string') return { u: units(value) }
    if (typeof value === 'number') return Number.isNaN(value) ? 'NaN' : value === Infinity ? 'Infinity' : value === -Infinity ? '-Infinity' : Object.is(value, -0) ? '-0' : value
    if (Array.isArray(value)) return Array.from(value, pack)
    if (value && typeof value === 'object') return Object.fromEntries(Object.entries(value).map(([key, item]) => [key, pack(item)]))
    return value
}
function failure(error: unknown) {
    return error instanceof Error ? { type: 'Error', name: error.name, message: units(error.message) }
        : { type: 'thrown-value', value: pack(error) }
}
function applySettings(settings: Settings) {
    const db = getDatabase() as unknown as Record<string, unknown>
    const custom = db.hypaCustomSettings as Record<string, unknown>
    const fields = { customUrl: 'url', customModel: 'model', customKey: 'key' } as const
    for (const [key, field] of Object.entries(fields)) {
        if (Object.hasOwn(settings, key)) {
            const value = optional(settings[key as keyof typeof fields])
            if (value === undefined) delete custom[field]
            else custom[field] = value
        }
    }
    if (Object.hasOwn(settings, 'supaKey')) {
        const value = optional(settings.supaKey)
        if (value === undefined) delete db.supaMemoryKey
        else db.supaMemoryKey = value
    }
    if (Object.hasOwn(settings, 'hypaModel')) {
        if (settings.hypaModel === undefined) delete db.hypaModel
        else db.hypaModel = settings.hypaModel
    }
}
function captured(processor: HypaProcesser) {
    const db = getDatabase()
    return { settings: {
        hypaModel: db.hypaModel === undefined ? { absent: true } : db.hypaModel,
        customUrl: encodedOptional(db.hypaCustomSettings?.url), customModel: encodedOptional(db.hypaCustomSettings?.model),
        customKey: encodedOptional(db.hypaCustomSettings?.key), supaKey: encodedOptional(db.supaMemoryKey),
    }, oaikey: encodedOptional(processor.oaikey) }
}
function headers(value: HeadersInit | undefined): [U16, U16][] {
    if (value instanceof Headers) return [...value].map(([name, value]) => [units(name), units(value)])
    if (Array.isArray(value)) return value.map(([name, value]) => [units(name), units(value)])
    return Object.entries(value ?? {}).map(([name, value]) => [units(name), units(String(value))])
}

afterEach(() => {
    expect(boundary.violations).toEqual([])
    boundary.handler = null
    vi.restoreAllMocks()
})
afterAll(() => {
    if (boundary.previous) Object.defineProperty(window, '__TAURI_INTERNALS__', boundary.previous)
    else delete (window as Window & { __TAURI_INTERNALS__?: unknown }).__TAURI_INTERNALS__
})

it('observes raw Hypa embedding calls and genuine native/plain fetch inputs', async () => {
    vi.spyOn(console, 'log').mockImplementation(() => {})
    vi.spyOn(console, 'warn').mockImplementation(() => {})
    vi.spyOn(console, 'error').mockImplementation(() => {})
    expect(isTauri).toBe(true)
    expect(boundary.violations).toEqual([])
    const reject = (message: string): never => { boundary.violations.push(message); throw new Error(message) }
    const checked = (assertion: () => void) => { try { assertion() } catch (error) { boundary.violations.push(String(error)); throw error } }
    vi.spyOn(globalThis, 'fetch').mockImplementation(async (input, init) => {
        if (!boundary.handler) return reject('Unplanned plain fetch before synthetic setup')
        return (boundary.handler as Transport)('plain', input, init)
    })
    const actualGlobalFetch = globalApi.globalFetch
    let raw: Raw[] = []
    let active = false
    vi.spyOn(globalApi, 'globalFetch').mockImplementation(async (url, args = {}) => {
        if (!active) return reject('Unplanned globalFetch outside embedding invocation')
        const body = JSON.stringify(args.body)
        if (body === undefined) return reject('Embedding globalFetch has no JSON body')
        raw.push({ constructedUrl: units(url), constructedUrlUsv: units(usv(url)), headers: headers(args.headers), bodyText: units(body), abortSignal: args.abortSignal ? 'present' : 'absent' })
        return actualGlobalFetch(url, args)
    })
    const set = readOracleCases<Entry>('providers/embeddings', 'remote')
    const results: unknown[] = []
    for (const entry of set.cases) {
        const input = entry.input
        setDatabase(normalizeDatabaseDefaults({ hypaCustomSettings: { url: '', model: '', key: '' }, usePlainFetch: input.transport === 'plain' } as unknown as Database))
        applySettings(input.construct.settings)
        const processor = new HypaProcesser(input.construct.model, optional(input.construct.customUrl))
        const snapshot = { model: processor.model, effectiveUrl: units(processor.customEmbeddingUrl),
            initialCacheIdentity: pack(hypaEmbeddingIdentity(processor.model, processor.customEmbeddingUrl, getDatabase().hypaCustomSettings?.model)) }
        const calls: unknown[] = []
        for (const call of input.calls) {
            if (call.before) applySettings(call.before)
            if (Object.hasOwn(call, 'oaikey')) {
                const key = optional(call.oaikey)
                if (key === undefined) delete processor.oaikey
                else processor.oaikey = key
            }
            raw = []
            const wire: unknown[] = []
            let consumed = false
            const capture = captured(processor)
            boundary.handler = (async (kind, url, init) => {
                checked(() => {
                    expect(active).toBe(true)
                    expect(kind).toBe(input.transport)
                    expect(consumed).toBe(false)
                    expect(raw).toHaveLength(1)
                    expect(String(url)).toBe(new URL(text(raw[0].constructedUrl)).href)
                    expect(init?.method).toBe('POST')
                    const body = kind === 'native' ? init?.dataText : init?.body
                    expect(typeof body).toBe('string')
                    expect(body).toBe(text(raw[0].bodyText))
                    expect(headers(init?.headers)).toEqual([[units('Content-Type'), units('application/json')], ...raw[0].headers])
                })
                consumed = true
                const body = String(kind === 'native' ? init?.dataText : init?.body)
                wire.push({ transport: kind, url: units(String(url)), method: init?.method,
                    headers: headers(init?.headers), bodyBytes: [...new TextEncoder().encode(body)] })
                if (call.duringReply) applySettings(call.duringReply)
                if (call.reply.kind === 'transport') {
                    const error = new Error(text(call.reply.message)); error.name = call.reply.name; throw error
                }
                return new Response(Uint8Array.from(call.reply.bytes), { status: call.reply.status, headers: { 'Content-Type': 'application/json' } })
            }) satisfies Transport
            active = true
            let outcome: unknown
            try {
                const value = call.input.shape === 'scalar' ? text(call.input.text) : call.input.texts.map(text)
                outcome = { vectors: pack(await processor.getEmbeds(value, call.purpose)) }
            } catch (error) { outcome = { failure: failure(error) } }
            active = false
            expect(boundary.violations, entry.id).toEqual([])
            const sourceBoundary = call.sourceBoundary ?? 'transport'
            expect(raw, entry.id).toHaveLength(sourceBoundary === 'before-request' ? 0 : 1)
            expect(wire, entry.id).toHaveLength(sourceBoundary === 'transport' ? 1 : 0)
            expect(consumed, entry.id).toBe(sourceBoundary === 'transport')
            calls.push({ captured: capture, purpose: call.purpose, raw, wire, reply: call.reply, outcome, afterReply: captured(processor) })
            boundary.handler = null
        }
        results.push({ boundary: 'hypa-remote-embedding-service', snapshot, calls })
    }
    expect(settleOracle(set, results, { generator: import.meta.url, tsSources: [
        'src/ts/process/memory/hypamemory.ts', 'src/ts/globalApi.svelte.ts', 'src/ts/util.ts',
        'src/ts/process/memory/hypaCacheKey.ts', 'src/ts/platform.ts',
    ] })).toMatch(/written|matched/)
    if (process.env.RUST_CORE_ORACLE_WRITE === '1') {
        const expected = JSON.parse(readFileSync(set.expectedPath, 'utf8')) as { tsSourceSha256: Record<string, string> }
        writeFileSync(join(dirname(set.expectedPath), 'engine.json'), `${JSON.stringify({ node: process.version, v8: process.versions.v8, unicode: process.versions.unicode, tsSourceSha256: expected.tsSourceSha256 }, null, 2)}\n`)
    }
}, 60000)
