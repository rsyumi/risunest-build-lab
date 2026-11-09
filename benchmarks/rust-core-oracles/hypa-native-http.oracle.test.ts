// @vitest-environment node
import { afterAll, afterEach, expect, it, vi } from 'vitest'
import 'fake-indexeddb/auto'
import { Window } from 'happy-dom'
import { createHash } from 'node:crypto'
import { readFileSync, writeFileSync } from 'node:fs'
import { dirname, join } from 'node:path'
import { readOracleCases, settleOracle } from './oracleFile'
import { testNetwork } from '../../tests/support/testNetwork'
import type { HypaModel } from '../../src/ts/process/memory/hypamemory'
import type { Database } from '../../src/ts/storage/database.svelte'

type PluginFetch = typeof import('@tauri-apps/plugin-http')['fetch']
const pluginObservers = vi.hoisted(() => ({
    before: null as null | ((...args: Parameters<PluginFetch>) => void),
    success: null as null | ((response: Response) => void),
    failure: null as null | ((error: unknown) => void),
}))
// Observation instrumentation only: all fetch processing remains in the installed module.
vi.mock('@tauri-apps/plugin-http', async (importOriginal) => {
    const actual = await importOriginal<typeof import('@tauri-apps/plugin-http')>()
    return { ...actual, fetch: (async (...args) => {
        pluginObservers.before?.(...args)
        try {
            const response = await actual.fetch(...args)
            pluginObservers.success?.(response)
            return response
        } catch (error) { pluginObservers.failure?.(error); throw error }
    }) satisfies PluginFetch }
})
type Text = string | { u: number[] }
type OptionalText = Text | { absent: true }
type Settings = { customUrl?: OptionalText; customModel?: OptionalText; customKey?: OptionalText; supaKey?: OptionalText; hypaModel?: HypaModel }
type Reply = { kind: 'http'; status: number; statusText?: string; headers: [Text, Text][]; chunks: number[][]; url?: Text }
    | { kind: 'ipc-error'; at: 'fetch' | 'send' | 'read'; message: Text }
type Call = { before?: Settings; oaikey?: OptionalText; purpose: 'document' | 'query'; input: { shape: 'scalar'; text: Text } | { shape: 'batch'; texts: Text[] }; reply: Reply; expect: { ipc: 'request' | 'none'; outcome: 'vectors' | 'failure' } }
type Entry = { id: string; input: { construct: { model: HypaModel | 'auto'; customUrl?: OptionalText; settings: Settings }; calls: Call[] } }
type ClientConfig = { method: string; url: string; headers: [string, string][]; data: number[] | null; dataText: string | null }
type Invocation = { reply: Reply; events: unknown[]; request?: ClientConfig; response?: unknown; cursor: number; requestRid: number; responseRid: number }

const text = (value: Text) => typeof value === 'string' ? value : value.u.map(unit => String.fromCharCode(unit)).join('')
const optional = (value: OptionalText | undefined) => value === undefined || typeof value === 'object' && 'absent' in value ? undefined : text(value)
const units = (value: string) => Array.from({ length: value.length }, (_, index) => value.charCodeAt(index))
function pack(value: unknown): unknown {
    if (value === undefined) return { undefined: true }
    if (typeof value === 'string') return { u: units(value) }
    if (typeof value === 'number') return Number.isNaN(value) ? 'NaN' : value === Infinity ? 'Infinity' : value === -Infinity ? '-Infinity' : Object.is(value, -0) ? '-0' : value
    if (Array.isArray(value)) return value.map(pack)
    if (value && typeof value === 'object') return Object.fromEntries(Object.entries(value).map(([key, item]) => [key, pack(item)]))
    return value
}
const failure = (error: unknown) => error instanceof Error
    ? { name: error.name, message: units(error.message) } : { value: pack(error) }
const headerEntries = (headers?: HeadersInit): [number[], number[]][] =>
    (headers instanceof Headers ? [...headers] : Array.isArray(headers) ? headers : Object.entries(headers ?? {}))
        .map(([name, value]) => [units(name), units(String(value))])

const native = { URL, Headers, Request, Response, ReadableStream, WritableStream, TransformStream, AbortController, AbortSignal }
const dom = new Window({ url: 'https://native-http-fixture.invalid/' })
type GuardRequest = { request: { url: string } }
dom.happyDOM.settings.fetch.interceptor = {
    beforeAsyncRequest: async ({ request }: GuardRequest) => { testNetwork.check(request.url) },
    beforeSyncRequest: ({ request }: GuardRequest) => { testNetwork.check(request.url) },
}
const previous = new Map<string, PropertyDescriptor | undefined>()
const uiGlobals = ['window', 'self', 'document', 'location', 'localStorage', 'sessionStorage', 'Window', 'HTMLElement', 'Element', 'Node', 'Document',
    'SVGElement', 'DOMParser', 'MutationObserver', 'ResizeObserver', 'IntersectionObserver', 'Image', 'FileReader', 'CustomEvent',
    'getComputedStyle', 'matchMedia', 'requestAnimationFrame', 'cancelAnimationFrame']
for (const key of uiGlobals) {
    previous.set(key, Object.getOwnPropertyDescriptor(globalThis, key))
    const value = key === 'window' || key === 'self' ? dom : (dom as unknown as Record<string, unknown>)[key]
    Object.defineProperty(globalThis, key, { configurable: true, writable: true, value })
}
// UI imports get a synthetic document; fetch semantics keep Node's genuine constructors.
dom.fetch = globalThis.fetch as unknown as typeof dom.fetch
for (const [key, value] of Object.entries(native)) Object.defineProperty(dom, key, { configurable: true, writable: true, value })
const violations: string[] = []
const reject = (message: string): never => { violations.push(message); throw new Error(message) }
const resources = new Map<number, Invocation>()
let active: Invocation | undefined
let rid = 0
const pending = new Set<Promise<unknown>>()

async function ipc(command: string, args: Record<string, unknown> = {}): Promise<unknown> {
    let invocation = command === 'plugin:http|fetch' ? active : resources.get(Number(args.rid))
    if (!invocation) return reject(`Unexpected IPC outside a synthetic HTTP invocation: ${command}`)
    if (command === 'plugin:http|fetch') {
        if (invocation.request) return reject('Duplicate fetch for one embedding call')
        const config = args.clientConfig as ClientConfig
        invocation.request = structuredClone(config)
        invocation.events.push({ command: 'fetch', clientConfig: pack(config) })
        if (invocation.reply.kind === 'ipc-error' && invocation.reply.at === 'fetch') throw new Error(text(invocation.reply.message))
        resources.set(invocation.requestRid, invocation)
        return invocation.requestRid
    }
    if (command === 'plugin:http|fetch_send') {
        invocation.events.push({ command: 'send' })
        if (invocation.reply.kind === 'ipc-error') {
            if (invocation.reply.at === 'send') throw new Error(text(invocation.reply.message))
        }
        const reply = invocation.reply.kind === 'http' ? invocation.reply : { status: 200, statusText: 'OK', headers: [] as [Text, Text][], url: undefined }
        const response = { status: reply.status, statusText: reply.statusText ?? '', url: reply.url === undefined ? invocation.request!.url : text(reply.url),
            headers: reply.headers.map(([name, value]) => [text(name), text(value)]), rid: invocation.responseRid }
        invocation.response = { status: response.status, statusText: response.statusText, url: response.url, headers: response.headers }
        resources.set(invocation.responseRid, invocation)
        return response
    }
    if (command === 'plugin:http|fetch_read_body') {
        if (invocation.reply.kind === 'ipc-error') {
            invocation.events.push({ command: 'read', failure: true })
            throw new Error(text(invocation.reply.message))
        }
        const chunk = invocation.reply.chunks[invocation.cursor++]
        const frame = chunk === undefined ? [1] : [...chunk, 0]
        invocation.events.push({ command: 'read', frame })
        return frame
    }
    if (command === 'plugin:http|fetch_cancel_body' || command === 'plugin:http|fetch_cancel') {
        invocation.events.push({ command: command.endsWith('fetch_cancel_body') ? 'cancel-body' : 'cancel' })
        return undefined
    }
    return reject(`Unexpected IPC command: ${command}`)
}
Object.defineProperty(dom, '__TAURI_INTERNALS__', { configurable: true, value: {
    metadata: { currentWindow: { label: 'synthetic' }, currentWebview: { label: 'synthetic' } },
    invoke: (command: string, args?: Record<string, unknown>) => {
        const operation = ipc(command, args)
        pending.add(operation)
        void operation.then(() => pending.delete(operation), () => pending.delete(operation))
        return operation
    },
    transformCallback: () => reject('Unexpected native callback registration'),
} })
async function drain() {
    await Promise.resolve()
    while (pending.size) await Promise.allSettled([...pending])
    await Promise.resolve()
}

afterEach(async () => {
    await drain()
    expect(violations).toEqual([])
    active = undefined
    pluginObservers.before = null
    pluginObservers.success = null
    pluginObservers.failure = null
    vi.restoreAllMocks()
})
afterAll(async () => {
    await dom.happyDOM.close()
    for (const [key, descriptor] of previous) {
        if (descriptor) Object.defineProperty(globalThis, key, descriptor)
        else Reflect.deleteProperty(globalThis, key)
    }
})

it('observes real Hypa globalFetch and installed plugin JS using Node fetch constructors with IPC only', async () => {
    expect({ URL, Headers, Request, Response, ReadableStream, WritableStream, TransformStream, AbortController, AbortSignal }).toEqual(native)
    vi.spyOn(console, 'log').mockImplementation(() => {})
    vi.spyOn(console, 'warn').mockImplementation(() => {})
    vi.spyOn(console, 'error').mockImplementation(() => {})
    let rawService: unknown[] = []
    let nativeFetchInput: unknown[] = []
    let jsResponse: unknown[] = []
    let fetchResult: unknown[] = []
    pluginObservers.before = (input, init) => {
        if (!active) return reject('Unplanned plugin fetch')
        nativeFetchInput.push({ url: units(String(input)), headers: headerEntries(init?.headers), dataText: pack(init?.dataText), method: init?.method })
    }
    pluginObservers.success = response => {
        jsResponse.push({ status: response.status, url: units(response.url), headers: headerEntries(response.headers), bodyNull: response.body === null })
    }
    pluginObservers.failure = error => { jsResponse.push({ failure: failure(error) }) }
    const globalApi = await import('../../src/ts/globalApi.svelte')
    const database = await import('../../src/ts/storage/database.svelte')
    const { HypaProcesser } = await import('../../src/ts/process/memory/hypamemory')
    const { hypaEmbeddingIdentity } = await import('../../src/ts/process/memory/hypaCacheKey')
    const { isTauri } = await import('../../src/ts/platform')
    expect(isTauri).toBe(true)
    expect({ URL, Headers, Request, Response, ReadableStream, WritableStream, TransformStream, AbortController, AbortSignal }).toEqual(native)
    const apply = (settings: Settings) => {
        const db = database.getDatabase()
        for (const [key, field] of Object.entries({ customUrl: 'url', customModel: 'model', customKey: 'key' })) {
            if (!Object.hasOwn(settings, key)) continue
            const value = optional(settings[key as 'customUrl' | 'customModel' | 'customKey'])
            if (value === undefined) delete db.hypaCustomSettings[field]
            else db.hypaCustomSettings[field] = value
        }
        if (Object.hasOwn(settings, 'supaKey')) {
            const value = optional(settings.supaKey)
            if (value === undefined) delete db.supaMemoryKey
            else db.supaMemoryKey = value
        }
        if (Object.hasOwn(settings, 'hypaModel')) db.hypaModel = settings.hypaModel!
    }
    const capture = (processor: InstanceType<typeof HypaProcesser>) => pack({ model: processor.model, effectiveUrl: processor.customEmbeddingUrl,
        customModel: database.getDatabase().hypaCustomSettings?.model, customKey: database.getDatabase().hypaCustomSettings?.key,
        supaKey: database.getDatabase().supaMemoryKey, oaikey: processor.oaikey })

    const actualGlobal = globalApi.globalFetch
    vi.spyOn(globalApi, 'globalFetch').mockImplementation(async (url, args = {}) => {
        if (!active) return reject('Unplanned globalFetch')
        rawService.push({ url: units(url), headers: headerEntries(args.headers), bodyText: units(JSON.stringify(args.body)), signal: args.abortSignal ? 'present' : 'absent' })
        const result = await actualGlobal(url, args)
        fetchResult.push(pack(result))
        return result
    })

    const set = readOracleCases<Entry>('providers/embeddings/native-http', 'intake')
    const results: unknown[] = []
    for (const entry of set.cases) {
        database.setDatabase(database.normalizeDatabaseDefaults({ usePlainFetch: false, hypaCustomSettings: { url: '', model: '', key: '' } } as unknown as Database))
        apply(entry.input.construct.settings)
        const processor = new HypaProcesser(entry.input.construct.model, optional(entry.input.construct.customUrl))
        const snapshot = { captured: capture(processor), cacheIdentity: pack(hypaEmbeddingIdentity(processor.model, processor.customEmbeddingUrl, database.getDatabase().hypaCustomSettings?.model)) }
        const calls: unknown[] = []
        for (const call of entry.input.calls) {
            if (call.before) apply(call.before)
            if (Object.hasOwn(call, 'oaikey')) {
                const value = optional(call.oaikey)
                if (value === undefined) delete processor.oaikey
                else processor.oaikey = value
            }
            rawService = []; nativeFetchInput = []; jsResponse = []; fetchResult = []
            const invocation: Invocation = { reply: call.reply, events: [], cursor: 0, requestRid: ++rid, responseRid: ++rid }
            active = invocation
            const captured = capture(processor)
            let outcome: unknown
            try { outcome = { vectors: pack(await processor.getEmbeds(call.input.shape === 'scalar' ? text(call.input.text) : call.input.texts.map(text), call.purpose)) } }
            catch (error) { outcome = { failure: failure(error) } }
            await drain()
            expect(rawService, entry.id).toHaveLength(1)
            expect(Boolean(invocation.request), entry.id).toBe(call.expect.ipc === 'request')
            expect(Object.hasOwn(outcome as object, 'vectors'), entry.id).toBe(call.expect.outcome === 'vectors')
            if (invocation.request) {
                expect(invocation.request.method, entry.id).toBe('POST')
                expect(invocation.request.data, entry.id).toBeNull()
                expect(units(invocation.request.dataText!), entry.id).toEqual((rawService[0] as { bodyText: number[] }).bodyText)
            }
            calls.push({ captured, purpose: call.purpose, rawService, nativeFetchInput, ipcRequest: pack(invocation.request),
                ipcResponse: pack(invocation.response), ipcEvents: invocation.events, jsResponse, globalFetch: fetchResult, outcome })
            active = undefined
        }
        results.push({ boundary: 'node-installed-plugin-ipc-observation', snapshot, calls })
    }
    expect(violations).toEqual([])
    settleOracle(set, results, { generator: import.meta.url, tsSources: [
        'src/ts/process/memory/hypamemory.ts', 'src/ts/globalApi.svelte.ts', 'src/ts/util.ts', 'src/ts/process/memory/hypaCacheKey.ts', 'src/ts/platform.ts',
        'node_modules/@tauri-apps/plugin-http/dist-js/index.js',
    ] })
    if (process.env.RUST_CORE_ORACLE_WRITE === '1') {
        const installed = readFileSync(join(set.root, 'node_modules/@tauri-apps/plugin-http/dist-js/index.js'))
        writeFileSync(join(dirname(set.expectedPath), 'engine.json'), `${JSON.stringify({ node: process.version, v8: process.versions.v8, unicode: process.versions.unicode,
            environment: 'node-native-fetch-with-synthetic-ui-dom', webViewEquivalent: false,
            installedPluginVersion: JSON.parse(readFileSync(join(set.root, 'node_modules/@tauri-apps/plugin-http/package.json'), 'utf8')).version,
            installedPluginRawSha256: createHash('sha256').update(installed).digest('hex'),
        }, null, 2)}\n`)
    }
}, 60000)
