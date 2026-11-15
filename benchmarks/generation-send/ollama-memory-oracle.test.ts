import { afterAll, expect, it, vi } from 'vitest'
import 'fake-indexeddb/auto'
import { mkdirSync, readFileSync, writeFileSync } from 'node:fs'
import { dirname, isAbsolute, relative, resolve, sep } from 'node:path'

// The request oracle has no mounted UI. Suspend application startup effects.
vi.mock('svelte/internal/client', async original => {
    const actual = await original<typeof import('svelte/internal/client')>()
    return { ...actual, effect_root: (_start: () => void) => () => {} }
})
import { normalizeDatabaseDefaults, setDatabaseLite, getCurrentCharacter, type Database } from '../../src/ts/storage/database.svelte'
import { deriveEffectivePersonaMirrors } from '../../src/ts/storage/effectiveIdentityState'
import { selectedCharID } from '../../src/ts/stores.svelte'
import { getModuleTriggers } from '../../src/ts/process/modules'
import { requestChatData } from '../../src/ts/process/request/request'
import { testNetwork } from '../../tests/support/testNetwork'

type Observation = { kind: 'resolved'; value: unknown } | { kind: 'thrown'; name: string; message: string; status_code?: number }
type ChatMessage = Parameters<typeof requestChatData>[0]['formated'][number]
type PortableMessage = Omit<ChatMessage, 'content' | 'name'> & { content?: string; contentUnits?: number[]; name?: string; nameUnits?: number[] }
type FixtureResponse = { status: number; reason: string; contentType: string; body: string }
type FixtureCase = {
    id: string; messages: PortableMessage[]; thinkingMode: string
    lifecycle?: 'pre-abort' | 'before-headers' | 'network-failure' | 'after-body-json'
    response?: FixtureResponse; reuseResponse?: FixtureResponse
    expected: ({ kind: 'resolved'; value: unknown } | { kind: 'thrown'; name: string; message?: string; status_code?: number }) & { requestCount: number; bodyText?: string; reuseBodyText?: string; reuseValue?: unknown }
}
type Fixture = { schema: number; sdkVersion: string; request: { model: string; selectedCharacterName: string; userName: string; temperature: number; maxResponse: number }; cases: FixtureCase[] }
const fixturePath = resolve('tests/fixtures/native-generation/ollama-memory.json')
const fixture = JSON.parse(readFileSync(fixturePath, 'utf8')) as Fixture
const sdk = JSON.parse(readFileSync(resolve('node_modules/ollama/package.json'), 'utf8')) as { version: string }
const records: unknown[] = []
const accepted = new Map<string, { outcome: Observation; requests: { url: string; bodyText: string }[]; reusedValue: unknown }>()
const writeFixture = process.env.RISUNEST_OLLAMA_MEMORY_WRITE_FIXTURE === '1'
const output = process.env.RISUNEST_OLLAMA_MEMORY_CAPTURE
let artifactPath: string | undefined
if (output) {
    const root = resolve('.tmp')
    const path = relative(root, resolve(output))
    if (!path || isAbsolute(path) || path === '..' || path.startsWith(`..${sep}`)) throw Error('Capture output must be a new file under .tmp')
    artifactPath = resolve(output)
}

const fromUnits = (units: number[]) => units.map(unit => String.fromCharCode(unit)).join('')
function message(value: PortableMessage): ChatMessage {
    const { contentUnits, nameUnits, ...rest } = value
    return { ...rest, content: contentUnits ? fromUnits(contentUnits) : rest.content ?? '',
        ...(nameUnits ? { name: fromUnits(nameUnits) } : {}) }
}
function expectedValue(value: unknown): unknown {
    if (!value || typeof value !== 'object') return value
    const { resultUnits, ...rest } = value as Record<string, unknown>
    return resultUnits ? { ...rest, result: fromUnits(resultUnits as number[]) } : rest
}

function portableValue(value: unknown): unknown {
    if (!value || typeof value !== 'object') return value
    const { result, ...rest } = value as Record<string, unknown>
    if (typeof result === 'string' && [...result].some(character => character.length === 1 && /[\uD800-\uDFFF]/.test(character))) {
        return { ...rest, resultUnits: Array.from({ length: result.length }, (_, index) => result.charCodeAt(index)) }
    }
    return { ...rest, result }
}

for (const row of fixture.cases) it(`actual hosted memory request: ${row.id}`, async () => {
    expect(fixture.schema).toBe(1)
    expect(sdk.version).toBe(fixture.sdkVersion)
    const database = normalizeDatabaseDefaults({
        aiModel: 'ollama-hosted', subModel: 'ollama-hosted', ollamaModel: fixture.request.model,
        ollamaModelSource: 'local', ollamaURL: 'http://127.0.0.1:11434', ollamaThinkingMode: row.thinkingMode,
        useStreaming: true, temperature: fixture.request.temperature, maxResponse: fixture.request.maxResponse,
        applyAdditionalParamsToAll: false, additionalParams: [], requestRetrys: 3, genTime: 1,
        selectedPersona: 0, personas: [{ id: 'selected', name: fixture.request.userName, personaPrompt: '', note: '', icon: '' }],
        characters: [{ type: 'character', chaId: 'synthetic-character', name: fixture.request.selectedCharacterName, chatPage: 0,
            chats: [{ id: 'synthetic-chat', name: 'Synthetic chat', message: [] }], customscript: [], triggerscript: [], globalLore: [], bias: [] }],
        botPresets: [], plugins: [], modules: [], enabledModules: [],
    } as unknown as Database)
    deriveEffectivePersonaMirrors(database)
    setDatabaseLite(database)
    selectedCharID.set(0)
    expect(database.characters[0].triggerscript).toEqual([])
    expect(database.modules).toEqual([])
    expect(database.enabledModules).toEqual([])
    expect(getCurrentCharacter().triggerscript).toEqual([])
    expect(getModuleTriggers()).toEqual([])
    const requests: { url: string; bodyText: string }[] = []
    const caller = new AbortController()
    let transportSignal: AbortSignal | undefined
    let requestSeen!: () => void
    const seen = new Promise<void>(resolve => { requestSeen = resolve })
    vi.spyOn(console, 'log').mockImplementation(() => {})
    const errors = vi.spyOn(console, 'error').mockImplementation(() => {})
    vi.spyOn(globalThis, 'fetch').mockImplementation(async (input, init) => {
        const url = typeof input === 'string' ? input : input instanceof URL ? input.href : input.url
        if (url !== 'http://127.0.0.1:11434/api/chat') { testNetwork.check(input); throw Error('Unexpected process boundary') }
        if (typeof init?.body !== 'string') throw Error('Expected SDK JSON body string')
        requests.push({ url, bodyText: init.body })
        transportSignal = init.signal as AbortSignal
        requestSeen()
        if (row.lifecycle === 'network-failure') throw new TypeError('synthetic network failure')
        if (row.lifecycle === 'before-headers') return await new Promise<Response>((_resolve, reject) => {
            transportSignal!.addEventListener('abort', () => reject(new DOMException('synthetic caller cancelled', 'AbortError')), { once: true })
        })
        const spec = requests.length === 2 && row.reuseResponse ? row.reuseResponse : row.response
        if (!spec) throw Error('Missing synthetic response')
        const response = new Response(spec.body, { status: spec.status, statusText: spec.reason,
            headers: { 'Content-Type': spec.contentType } })
        if (row.lifecycle === 'after-body-json') {
            const original = response.json.bind(response)
            response.json = async () => { const value = await original(); caller.abort(); return value }
        }
        return response
    })
    let timer: ReturnType<typeof setTimeout> | undefined
    const guard = new Promise<never>((_resolve, reject) => { timer = setTimeout(() => reject(Error('EXTERNAL COMPLETION GUARD EXPIRED')), 5000) })
    let outcome: Observation
    try {
        if (row.lifecycle === 'pre-abort') caller.abort()
        const pending = requestChatData({ formated: row.messages.map(message), bias: {}, tools: [], useStreaming: false }, 'memory', caller.signal)
        if (row.lifecycle === 'before-headers') {
            await Promise.race([seen, pending.then(() => { throw Error('Settled before cancellation') }), guard])
            caller.abort()
        }
        try { outcome = { kind: 'resolved', value: await Promise.race([pending, guard]) } }
        catch (caught) {
            const error = caught as Error & { status_code?: number }
            outcome = { kind: 'thrown', name: error.name, message: error.message,
                ...(error.status_code === undefined ? {} : { status_code: error.status_code }) }
        }
        let reusedValue: unknown
        if (row.reuseResponse) {
            expect(transportSignal?.aborted).toBe(true)
            if (outcome.kind !== 'resolved') throw Error('First summary threw')
            const first = outcome.value as { type: string; result: string }
            if (first.type !== 'success' || typeof first.result !== 'string') throw Error('First summary was not text')
            reusedValue = await Promise.race([requestChatData({ formated: [
                { role: 'user', content: first.result }, { role: 'system', content: 'Summarize again.' },
            ], bias: {}, tools: [], useStreaming: false }, 'memory', caller.signal), guard])
            expect(reusedValue).toEqual(expectedValue(row.expected.reuseValue))
        }
        const consoleErrors = errors.mock.calls.map(args => {
            const error = args[0] as Error
            return { argumentCount: args.length, name: error?.name, message: error?.message,
                expectedRequestFrame: typeof error?.stack === 'string' && error.stack.includes('requestChatDataUnderHold') && error.stack.includes('src/ts/process/request/request.ts') }
        })
        records.push({ id: row.id, requests, outcome, reusedValue, transportAborted: transportSignal?.aborted, consoleErrors })
        if (row.lifecycle === 'pre-abort') expect(consoleErrors).toEqual([])
        // The current no-hooks path logs this caught TypeError. Its absence after a fix is also valid.
        for (const error of consoleErrors) expect(error).toEqual({ argumentCount: 1, name: 'TypeError',
            message: "Cannot read properties of null (reading 'displayData')", expectedRequestFrame: true })
        expect(requests).toHaveLength(row.expected.requestCount)
        expect(transportSignal?.aborted).toBe(row.lifecycle === 'pre-abort' ? undefined : true)
        if (row.expected.bodyText) expect(requests[0].bodyText).toBe(row.expected.bodyText)
        if (row.expected.reuseBodyText) expect(requests[1].bodyText).toBe(row.expected.reuseBodyText)
        if (row.expected.kind === 'resolved') expect(outcome).toEqual({ kind: 'resolved', value: expectedValue(row.expected.value) })
        else expect(outcome).toMatchObject({ kind: 'thrown', name: row.expected.name,
            ...(row.expected.message === undefined ? {} : { message: row.expected.message }),
            ...(row.expected.status_code === undefined ? {} : { status_code: row.expected.status_code }) })
        accepted.set(row.id, { outcome, requests, reusedValue })
    } finally { clearTimeout(timer); caller.abort(); vi.restoreAllMocks() }
}, 12000)

afterAll(() => {
    if (writeFixture) {
        if (accepted.size !== fixture.cases.length) throw Error('Refusing to write a partial or failed memory fixture capture')
        for (const row of fixture.cases) {
            const captured = accepted.get(row.id)!
            row.expected.requestCount = captured.requests.length
            if (captured.requests[0]) row.expected.bodyText = captured.requests[0].bodyText
            if (captured.requests[1]) row.expected.reuseBodyText = captured.requests[1].bodyText
            if (row.expected.kind === 'resolved' && captured.outcome.kind === 'resolved') row.expected.value = portableValue(captured.outcome.value)
            if (row.expected.reuseValue !== undefined) row.expected.reuseValue = portableValue(captured.reusedValue)
        }
        writeFileSync(fixturePath, JSON.stringify(fixture, null, 2) + '\n')
    }
    if (!artifactPath) return
    mkdirSync(dirname(artifactPath), { recursive: true })
    writeFileSync(artifactPath, JSON.stringify({ schema: 1, sdkVersion: sdk.version, authority: 'application-transform; Fetch diagnostics are boundary-controlled',
        expectedCases: fixture.cases.length, records }, null, 2) + '\n', { flag: 'wx' })
})
