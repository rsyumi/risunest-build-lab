import { afterEach, describe, expect, it, vi } from 'vitest'
import 'fake-indexeddb/auto'
import { createHash } from 'node:crypto'
import { setImmediate as immediate } from 'node:timers/promises'
import { historyFixture, bufferedResponse, countTokens } from '../generation-memory/fixture'

// Vitest's asynchronous module graph can schedule Svelte effects before cyclic
// imports initialize. Run the real roots once imports and the synthetic DB exist.
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

import { normalizeDatabaseDefaults, setDatabase, getDatabase } from '../../src/ts/storage/database.svelte'
import { selectedCharID } from '../../src/ts/stores.svelte'
import { characterFormatUpdate } from '../../src/ts/characters'
import { getPersistentDataStore } from '../../src/ts/storage/persistentDataStoreFactory'
import { getPersistentDataRuntime } from '../../src/ts/storage/persistentDataRuntime.svelte'
import { projectCompleteScalableWorkingSet } from '../../src/ts/storage/workingSetCatalog'
import { pluginV2 } from '../../src/ts/plugins/plugins.svelte'
import { sendChat, type OpenAIChat } from '../../src/ts/process/index.svelte'
import { ChatTokenizer, encodeWithTokenizer } from '../../src/ts/tokenizer'
import { updateDeviceSettings } from '../../src/ts/storage/deviceSettings'

afterEach(() => {
    for (const dispose of startup.dispose.splice(0)) dispose()
    pluginV2.providers.delete('synthetic-provider')
    pluginV2.providerOptions.delete('synthetic-provider')
    delete (globalThis as any).__generationSendProbe
    delete (globalThis as any).__generationSendZone
    vi.restoreAllMocks()
})

describe('genuine synthetic send pipeline', () => {
    it('runs real preparation and persists a buffered synthetic provider response', async () => {
        const probeMode = process.env.GENERATION_SEND_PROBES || 'fine'
        if (probeMode !== 'coarse' && probeMode !== 'fine') throw new Error('Expected coarse or fine probes')
        const fineProbes = probeMode === 'fine'
        let pretrimLogCharacters = 0
        let pretrimLogUtf8Bytes = 0
        vi.spyOn(console, 'log').mockImplementation(fineProbes ? (value) => {
            if ((globalThis as any).__generationSendZone === 'pretrim-prompt-log-serialization' && typeof value === 'string') {
                pretrimLogCharacters += value.length
                pretrimLogUtf8Bytes += Buffer.byteLength(value)
            }
        } : () => {})
        vi.spyOn(console, 'debug').mockImplementation(() => {})
        const historyLimit = process.env.GENERATION_SEND_WINDOW === '1'
        const fixture = historyFixture(Number(process.env.GENERATION_SEND_TOKENS || 1000))
        const expectedHistory = fixture.chat.message.map(({ data, chatId, role }) => ({ data, chatId, role }))
        const database = normalizeDatabaseDefaults(fixture.database)
        Object.assign(database.characters[0], { globalLore: [], bias: [], desc: '', firstMessage: '', newGenData: {} })
        characterFormatUpdate(database.characters[0])
        Object.assign(database, { aiModel: 'custom', currentPluginProvider: 'synthetic-provider', maxContext: 128000,
            maxResponse: 20000, useTokenizerCaching: false, promptTemplate: null, mainPrompt: 'Synthetic assistant.',
            jailbreak: '', globalNote: '', formatingOrder: ['main', 'description', 'chats', 'lastChat'] })
        const templateEnabled = process.env.GENERATION_SEND_TEMPLATE === '1'
        if (templateEnabled) database.promptTemplate = [
            { type: 'plain', text: 'Synthetic assistant.', role: 'system', type2: 'main' },
            { type: 'description' }, { type: 'lorebook' },
            { type: 'chat', rangeStart: 0, rangeEnd: 'end' }, { type: 'postEverything' },
        ]
        const store = getPersistentDataStore()
        await store.open()
        const { revision } = await store.replaceFromDatabase(database)
        setDatabase(projectCompleteScalableWorkingSet(database, 'character', revision, new Set(['character'])))
        selectedCharID.set(0)
        startup.ready = true
        for (const activate of startup.roots.splice(0)) activate()
        const runtime = getPersistentDataRuntime()
        await runtime.initializeActiveWorkingSet(getDatabase())
        await runtime.flushPendingDataLocally('synthetic-setup')
        expect(runtime.getSelectedConversationMode()).toBe('windowed')
        updateDeviceSettings({ generationHistoryLimitEnabled: true, generationHistoryLimitMultiplier: 2 })
        expect(pluginV2.chatOutput.size).toBe(0)
        const response = bufferedResponse()
        let requested = 0
        let providerAt = 0
        let prompt: OpenAIChat[] = []
        let saveRouteAtProvider: string | null = null
        const phases: Record<string, { calls: number, elapsedMs: number, preparationMs: number, completionMs: number }> = {}
        let measuring = false
        const encodingByZone: Record<string, { calls: number, elapsedMs: number, characters: number }> = {}
        const span = (name: string, start: number, end: number, characters = 0) => {
            if (!measuring) return
            const phase = phases[name] ??= { calls: 0, elapsedMs: 0, preparationMs: 0, completionMs: 0 }
            if (fineProbes && name === 'tokenizer') {
                const zone = `${providerAt ? 'completion' : 'preparation'}:${(globalThis as any).__generationSendZone ?? 'other'}`
                const encoding = encodingByZone[zone] ??= { calls: 0, elapsedMs: 0, characters: 0 }
                encoding.calls++
                encoding.elapsedMs += end - start
                encoding.characters += characters
            }
            phase.calls++
            phase.elapsedMs += end - start
            if (!fineProbes) phase[providerAt ? 'completionMs' : 'preparationMs'] += end - start
            else if (providerAt) {
                phase.preparationMs += Math.max(0, Math.min(end, providerAt) - start)
                phase.completionMs += Math.max(0, end - Math.max(start, providerAt))
            } else phase.preparationMs += end - start
        }
        ;(globalThis as any).__generationSendProbe = span
        const tokenCalls: Record<string, { calls: number, unchangedObjectRecounts: number, characters: number, elapsedMs: number }> = {}
        const previousCounts = new WeakMap<ChatTokenizer, WeakMap<OpenAIChat, { content: string, name: string | undefined }>>()
        const tokenizeChat = ChatTokenizer.prototype.tokenizeChat
        if (fineProbes) vi.spyOn(ChatTokenizer.prototype, 'tokenizeChat').mockImplementation(async function(this: ChatTokenizer, data, args) {
            if (!measuring) return tokenizeChat.call(this, data, args)
            const zone = (globalThis as any).__generationSendZone ?? 'other'
            const calls = tokenCalls[zone] ??= { calls: 0, unchangedObjectRecounts: 0, characters: 0, elapsedMs: 0 }
            const previous = previousCounts.get(this) ?? new WeakMap()
            previousCounts.set(this, previous)
            const old = previous.get(data)
            calls.calls++
            calls.characters += data.content.length
            if (old?.content === data.content && old.name === data.name && !data.multimodals?.length && !args?.countThoughts) {
                calls.unchangedObjectRecounts++
            }
            const content = data.content
            const name = data.name
            const start = performance.now()
            try { return await tokenizeChat.call(this, data, args) }
            finally {
                calls.elapsedMs += performance.now() - start
                if (!data.multimodals?.length && !args?.countThoughts) previous.set(data, { content, name })
            }
        })
        const acquire = runtime.acquireCompleteConversation.bind(runtime)
        vi.spyOn(runtime, 'acquireCompleteConversation').mockImplementation(async (...args) => {
            const start = performance.now()
            try { return await acquire(...args) }
            finally { span('complete-promotion', start, performance.now()) }
        })
        const commit = store.commit.bind(store)
        let commitCount = 0
        vi.spyOn(store, 'commit').mockImplementation(async (...args) => {
            const start = performance.now()
            try { return await commit(...args) }
            finally { span('indexeddb-commit', start, performance.now()); if (measuring) commitCount++ }
        })
        pluginV2.providerOptions.set('synthetic-provider', { tokenizer: 'cl100k_base' })
        pluginV2.providers.set('synthetic-provider', async (arg) => {
            requested++
            providerAt = performance.now()
            prompt = arg.prompt_chat
            saveRouteAtProvider = runtime.getSelectedConversationMode()
            return { success: true, content: response }
        })
        // Isolate one-time WASM initialization from the size-dependent send path.
        await encodeWithTokenizer('synthetic-tokenizer-warmup', 'tik')
        await immediate()
        let lastTick = performance.now()
        let maxEventLoopGapMs = 0
        let peakObservedHeapBytes = process.memoryUsage().heapUsed
        const timer = setInterval(() => {
            const now = performance.now()
            maxEventLoopGapMs = Math.max(maxEventLoopGapMs, now - lastTick)
            lastTick = now
            peakObservedHeapBytes = Math.max(peakObservedHeapBytes, process.memoryUsage().heapUsed)
        }, 1)
        const heapBeforeBytes = process.memoryUsage().heapUsed
        const start = performance.now()
        measuring = true
        let success: boolean
        try { success = await sendChat({ historyLimit }) }
        catch (error) { clearInterval(timer); throw error }
        finally { measuring = false }
        const end = performance.now()
        maxEventLoopGapMs = Math.max(maxEventLoopGapMs, end - lastTick)
        peakObservedHeapBytes = Math.max(peakObservedHeapBytes, process.memoryUsage().heapUsed)
        await immediate()
        clearInterval(timer)
        expect(success).toBe(true)
        expect(requested).toBe(1)
        expect(saveRouteAtProvider).toBe(historyLimit ? 'windowed' : 'complete')
        const saved = (await store.readConversation('character', 'conversation'))!.value
        expect(saved.message).toHaveLength(fixture.metrics.messages + 1)
        expect(saved.message.at(-1)?.data).toBe(response)
        expect(saved.message.slice(0, -1).map(({ data, chatId, role }) => ({ data, chatId, role }))).toEqual(expectedHistory)
        const inputTokens = saved.message.at(-1)?.generationInfo?.inputTokens ?? null
        expect(inputTokens).not.toBeNull()
        expect(inputTokens! + 20000).toBeLessThanOrEqual(128000)
        process.stdout.write('GENERATION_SEND ' + JSON.stringify({ ...fixture.metrics, historyLimit, templateEnabled, probeMode,
            generationRoute: historyLimit ? 'history-window' : 'complete', saveRouteAtProvider,
            configuredContext: 128000, responseTokens: countTokens(response), tokenizerStringCache: false,
            totalMs: end - start, preparationMs: providerAt - start, completionMs: end - providerAt,
            inputTokens, promptMessages: prompt.length,
            promptCharacters: prompt.reduce((n, message) => n + message.content.length, 0),
            phases, ...(fineProbes ? { tokenCalls, encodingByZone, pretrimLogCharacters, pretrimLogUtf8Bytes } : {}), promptSha256: createHash('sha256').update(JSON.stringify(prompt)).digest('hex'), commitCount, commitJsonBytes: null, maxEventLoopGapMs,
            heapBeforeBytes, peakObservedHeapBytes, rendererPeakMemory: null,
            platform: process.platform, node: process.version,
            scope: 'Node happy-dom, real send/preparation/completion and IndexedDb adapter; fake IndexedDB I/O; no mounted UI or native tokenizer/SQLite',
        }) + '\n')
    }, 120000)
})
