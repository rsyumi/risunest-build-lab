import { afterAll, beforeAll, expect, it, vi } from 'vitest'
import 'fake-indexeddb/auto'
import { createHash } from 'node:crypto'
import { mkdirSync, readFileSync, writeFileSync } from 'node:fs'
import { dirname, isAbsolute, relative, resolve, sep } from 'node:path'

// This oracle has no mounted UI. Application startup effects remain suspended.
vi.mock('svelte/internal/client', async (original) => {
    const actual = await original<typeof import('svelte/internal/client')>()
    return { ...actual, effect_root: (_start: () => void) => () => {} }
})

import { normalizeDatabaseDefaults, setDatabaseLite, getDatabase, type Database } from '../../src/ts/storage/database.svelte'
import { deriveEffectivePersonaMirrors } from '../../src/ts/storage/effectiveIdentityState'
import { selectedCharID } from '../../src/ts/stores.svelte'
import { requestChatDataMain, type requestDataResponse } from '../../src/ts/process/request/request'
import { allowTestLoopbackOrigin } from '../../tests/support/testNetwork'
import { createOllamaFixtureServer, encodeJsText as jsText, encodeDiagnosticValue } from './ollama-browser-oracle.mjs'

type BytePart = { utf8: string; hex?: never } | { hex: string; utf8?: never }
type JsText = string | { utf16: number[] }
type Lifecycle = 'pre-abort' | 'before-headers' | 'caller-cancel' | 'consumer-cancel' | 'body-failure' | 'terminal-open'
type Observation = {
    outcome: 'success' | 'stream-complete' | 'consumer-cancelled' | 'error'
    result?: unknown
    snapshots: JsText[]
    error?: { name: string; message: JsText; statusCode?: number; sdkError?: unknown }
}
type FixtureCase = {
    id: string; stream?: boolean; thinkingMode?: string; expectedThink?: boolean | string
    boundPersona?: boolean; hostTemplate?: string; expectedPath?: string
    status?: number; statusText?: string; contentType?: string; encoding?: 'gzip' | 'deflate' | 'br' | 'zstd'
    body: BytePart[]; fragmentBytes?: number; redirect?: number; lifecycle?: Lifecycle
    encodings?: ('gzip' | 'deflate' | 'deflate-raw' | 'br' | 'zstd')[]; contentEncoding?: string | string[] | null
    gzipMemberSplitAt?: number; truncateBytes?: number; corruptGzipCrc32?: boolean; zstdMemberSplitAt?: number; zstdSecondWindowLog?: number; zstdSkippable?: boolean; zstdWindowLog?: number; redirectLoop?: boolean
    gate?: string; expectedRequests?: number
    expected?: {
        outcome: Observation['outcome']; result?: unknown; snapshots?: JsText[]
        errorName?: string; errorMessage?: JsText; statusCode?: number; sdkError?: unknown
    }
}
type Fixture = {
    schema: number; phase: string; sdkVersion: string
    request: {
        model: string; characterName: string; selectedUserName: string; boundUserName: string
        messages: { role: 'system' | 'user' | 'assistant' | 'function'; content: string; name?: string }[]
        wireMessages: { role: string; content: string }[]
    }
    cases: FixtureCase[]
}
type CapturedRequest = { method: string; path: string; headers: Record<string, string>; body: unknown }

const fixtureSource = readFileSync(resolve('tests/fixtures/native-generation/ollama-transport.json'), 'utf8')
const fixture = JSON.parse(fixtureSource) as Fixture
const sdkPackage = JSON.parse(readFileSync(resolve('node_modules/ollama/package.json'), 'utf8')) as { version: string }
const records: { id: string; gate?: string; authority: 'application-transform' | 'fetch-emulator-only'; requests: CapturedRequest[]; preflights: unknown[]; responseEvidence: unknown; responseClosedWithoutEnd: boolean; observation: Observation }[] = []
const outputRoot = resolve('.tmp/native-ollama-transport')
const output = process.env.RISUNEST_OLLAMA_TRANSPORT_ORACLE
let artifactPath: string | undefined

function observedError(error: unknown): NonNullable<Observation['error']> {
    const value = error as { name?: unknown; message?: unknown; status_code?: unknown; error?: unknown } | null
    return {
        name: typeof value?.name === 'string' ? value.name : typeof error,
        message: jsText(typeof value?.message === 'string' ? value.message : String(error)),
        ...(typeof value?.status_code === 'number' ? { statusCode: value.status_code } : {}),
        ...(value && Object.hasOwn(value, 'error') ? { sdkError: encodeDiagnosticValue(value.error) } : {}),
    }
}

const loopback = (entry: FixtureCase) => createOllamaFixtureServer(entry, { allowOrigin: allowTestLoopbackOrigin, corsOrigin: globalThis.location.origin })

function configure(entry: FixtureCase, host: string) {
    const request = fixture.request
    const database = normalizeDatabaseDefaults({
        aiModel: 'ollama-hosted', ollamaModel: request.model, ollamaModelSource: 'local', ollamaURL: host,
        ollamaThinkingMode: entry.thinkingMode ?? 'auto', useStreaming: entry.stream ?? false,
        applyAdditionalParamsToAll: false, additionalParams: [], requestRetrys: 0, genTime: 1,
        selectedPersona: 0, personas: [
            { id: 'selected', name: request.selectedUserName, personaPrompt: '', note: '', icon: '' },
            { id: 'bound', name: request.boundUserName, personaPrompt: '', note: '', icon: '' },
        ],
        characters: [{ type: 'character', chaId: 'character', name: request.characterName, chatPage: 0,
            chats: [{ id: 'conversation', name: 'Synthetic conversation', message: [], ...(entry.boundPersona ? { bindedPersona: 'bound' } : {}) }],
            customscript: [], triggerscript: [], globalLore: [], bias: [] }],
        botPresets: [], plugins: [], modules: [], enabledModules: [],
    } as unknown as Database)
    deriveEffectivePersonaMirrors(database)
    setDatabaseLite(database)
    selectedCharID.set(0)
}

async function observe(entry: FixtureCase, controls: Awaited<ReturnType<typeof loopback>>): Promise<Observation> {
    const host = (entry.hostTemplate ?? '{origin}').replaceAll('{origin}', controls.origin).replaceAll('{port}', String(controls.port))
    configure(entry, host)
    const cancellation = new AbortController()
    const snapshots: JsText[] = []
    let reader: ReadableStreamDefaultReader<Record<string, string>> | undefined
    if (entry.lifecycle === 'pre-abort') cancellation.abort()
    const pending = requestChatDataMain({
        formated: structuredClone(fixture.request.messages), bias: {}, currentChar: getDatabase().characters[0],
        staticModel: 'ollama-hosted', useStreaming: true,
    }, 'model', cancellation.signal)
    void pending.catch(() => {})
    try {
        if (entry.lifecycle === 'before-headers') {
            await Promise.race([
                controls.requestSeen.promise,
                pending.then(() => { throw new Error('Request settled before the held-header condition') }),
            ])
            cancellation.abort()
        }
        const response: requestDataResponse = await pending
        if (response.type === 'success') return { outcome: 'success', result: encodeDiagnosticValue(response.result), snapshots }
        if (response.type !== 'streaming') throw new Error(`Unexpected adapter response: ${response.type}`)
        reader = response.result.getReader()
        while (true) {
            const next = await reader.read()
            if (next.done) break
            snapshots.push(jsText(next.value['0']))
            if (snapshots.length === 1) {
                if (entry.lifecycle === 'caller-cancel') cancellation.abort()
                if (entry.lifecycle === 'body-failure') controls.release.resolve()
                if (entry.lifecycle === 'consumer-cancel') {
                    await reader.cancel()
                    await controls.closed.promise
                    return { outcome: 'consumer-cancelled', snapshots }
                }
            }
        }
        if (entry.lifecycle === 'terminal-open') {
            await controls.closed.promise
            expect(controls.responseClosedWithoutEnd).toBe(true)
        }
        return { outcome: 'stream-complete', snapshots }
    } catch (error) {
        return { outcome: 'error', snapshots, error: observedError(error) }
    } finally {
        cancellation.abort()
        reader?.releaseLock()
    }
}

function assertObservation(entry: FixtureCase, observation: Observation) {
    const expected = entry.expected
    if (!expected) {
        expect(entry.gate, 'Unfrozen behavior must name its exactness gate').toEqual(expect.any(String))
        return
    }
    expect(observation.outcome).toBe(expected.outcome)
    if (Object.hasOwn(expected, 'result')) expect(observation.result).toEqual(expected.result)
    if (expected.snapshots) expect(observation.snapshots).toEqual(expected.snapshots)
    if (expected.errorName) expect(observation.error?.name).toBe(expected.errorName)
    if (Object.hasOwn(expected, 'errorMessage')) expect(observation.error?.message).toEqual(expected.errorMessage)
    if (expected.statusCode !== undefined) expect(observation.error?.statusCode).toBe(expected.statusCode)
    if (Object.hasOwn(expected, 'sdkError')) expect(observation.error?.sdkError).toEqual(expected.sdkError)
}

function prerequisites(): string {
    if (sdkPackage.version !== fixture.sdkVersion || fixture.sdkVersion !== '0.6.4') throw new Error('Expected installed browser SDK 0.6.4')
    if (fixture.schema !== 1) throw new Error('Unexpected transport fixture schema')
    if (new Set(fixture.cases.map(entry => entry.id)).size !== fixture.cases.length) throw new Error('Duplicate transport fixture IDs')
    if (!output) throw new Error('Set RISUNEST_OLLAMA_TRANSPORT_ORACLE to a file under .tmp/native-ollama-transport')
    const path = relative(outputRoot, resolve(output))
    if (!path || isAbsolute(path) || path === '..' || path.startsWith(`..${sep}`) || resolve(output) === outputRoot) throw new Error('Oracle output must be a file under .tmp/native-ollama-transport')
    return resolve(output)
}

function authority(entry: FixtureCase): 'application-transform' | 'fetch-emulator-only' {
    const gate = entry.gate ?? ''
    return /^(fetch-|content-encoding|http-status-text|http-content-type|abort-diagnostics|network-diagnostics|json-error-diagnostics|partial-output-error-order)/.test(gate)
        ? 'fetch-emulator-only' : 'application-transform'
}

beforeAll(() => { artifactPath = prerequisites() })

for (const entry of fixture.cases) {
    it(`characterizes actual built-in Ollama HTTP: ${entry.id}`, async ({ onTestFinished }) => {
        vi.spyOn(console, 'log').mockImplementation(() => {})
        vi.spyOn(console, 'warn').mockImplementation(() => {})
        prerequisites()
        const controls = await loopback(entry)
        onTestFinished(() => controls.dispose())
        try {
            const observation = await observe(entry, controls)
            controls.assertHealthy()
            records.push({ id: entry.id, gate: entry.gate, authority: authority(entry), requests: controls.requests, preflights: controls.preflights, responseEvidence: controls.responseEvidence,
                responseClosedWithoutEnd: controls.responseClosedWithoutEnd, observation })
            if (entry.expectedRequests !== undefined) expect(controls.requests).toHaveLength(entry.expectedRequests)
            else if (entry.id !== 'host-userinfo' && !entry.redirectLoop) expect(controls.requests).toHaveLength(entry.redirect ? 2 : 1)
            const first = controls.requests[0]
            if (first) {
                expect(first.method).toBe('POST')
                expect(first.path).toBe(entry.expectedPath ?? '/api/chat')
                expect(first.headers['content-type']).toBe('application/json')
                expect(first.headers.accept).toBe('application/json')
                if (entry.id !== 'host-userinfo') expect(first.headers).not.toHaveProperty('authorization')
                expect(first.body).toEqual({ model: fixture.request.model, messages: fixture.request.wireMessages,
                    stream: entry.stream ?? false, ...(Object.hasOwn(entry, 'expectedThink') ? { think: entry.expectedThink } : {}) })
            }
            assertObservation(entry, observation)
        } finally {
            await controls.dispose()
            vi.restoreAllMocks()
        }
    }, 30000)
}

afterAll(() => {
    if (!artifactPath) return
    mkdirSync(dirname(artifactPath), { recursive: true })
    writeFileSync(artifactPath, JSON.stringify({
        schema: 1, phase: 'unfrozen-characterization', fixturePhase: fixture.phase,
        fixtureSha256: createHash('sha256').update(fixtureSource).digest('hex'), sdkVersion: sdkPackage.version,
        sdkSha256: createHash('sha256').update(readFileSync(resolve('node_modules/ollama/dist/browser.mjs'))).digest('hex'),
        runtime: { node: process.versions.node, fetch: 'Vitest happy-dom Fetch, actual owned-loopback HTTP' },
        proofBoundary: { nativeAcceptance: false, fetchGoldensAllowed: false, requires: 'Root-reviewed actual Chromium plus SDK diagnostics before Fetch observations become browser/native goldens' },
        complete: records.length === fixture.cases.length, cases: records,
    }, null, 2) + '\n')
})
