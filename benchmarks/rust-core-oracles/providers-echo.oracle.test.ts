import { afterEach, expect, it, vi } from 'vitest'
import 'fake-indexeddb/auto'
import { readOracleCases, settleOracle, type OracleCase } from './oracleFile'

// This oracle has no mounted UI. Application startup effects remain suspended.
vi.mock('svelte/internal/client', async (original) => {
    const actual = await original<typeof import('svelte/internal/client')>()
    return { ...actual, effect_root: (_start: () => void) => () => {} }
})

import { normalizeDatabaseDefaults, setDatabaseLite, type Database } from '../../src/ts/storage/database.svelte'
import { requestChatDataMain } from '../../src/ts/process/request/request'
import { getModelInfo } from '../../src/ts/model/modellist'

type Case = OracleCase & { db?: { echoMessage?: string | null; echoDelay?: number | null } }

// Code-unit arrays preserve even isolated surrogates in the result comparison.
const units = (text: string) => Array.from({ length: text.length }, (_, index) => text.charCodeAt(index))

afterEach(() => {
    vi.restoreAllMocks()
    vi.useRealTimers()
})

it('observes Echo text and availability through the actual catalog route and timer boundary', async () => {
    const set = readOracleCases<Case>('providers/echo', 'results')
    const results: unknown[] = []
    for (const entry of set.cases) {
        const database = normalizeDatabaseDefaults({
            aiModel: 'gpt35', characters: [], botPresets: [], plugins: [], modules: [], enabledModules: [],
        } as unknown as Database)
        // Exercise requestEcho's own defaults after normal startup normalization.
        delete database.echoMessage
        delete database.echoDelay
        Object.assign(database, entry.db)
        setDatabaseLite(database)
        vi.useFakeTimers({ toFake: ['setTimeout', 'clearTimeout', 'Date'] })
        vi.setSystemTime(0)
        const timer = vi.spyOn(globalThis, 'setTimeout')
        let observed: { type: string; textUnits: number[]; availableAtMs: number } | undefined
        const pending = requestChatDataMain({
            staticModel: 'echo_model', formated: [{ role: 'user', content: 'Synthetic input, not the Echo result.' }], bias: {},
        }, 'model').then(response => {
            expect(response.type).toBe('success')
            expect(typeof response.result).toBe('string')
            observed = { type: response.type, textUnits: units(response.result as string), availableAtMs: Date.now() }
        })
        // Fake-clock availability and elapsed time are timer-boundary evidence. The pure plan
        // preserves the requested milliseconds before the timer applies rounding or clamping.
        await vi.advanceTimersByTimeAsync(0)
        const availableBeforeTimer = observed !== undefined
        const requestedDelaysMs = timer.mock.calls.map(call => call[1])
        expect(requestedDelaysMs.length).toBeLessThanOrEqual(1)
        await vi.runAllTimersAsync()
        await pending
        const model = getModelInfo('echo_model')
        results.push({ model: { id: model.id, format: model.format }, requestedDelaysMs, availableBeforeTimer, outcome: observed })
        timer.mockRestore()
        vi.useRealTimers()
    }
    settleOracle(set, results, {
        generator: import.meta.url,
        tsSources: ['src/ts/process/request/request.ts', 'src/ts/util.ts', 'src/ts/model/modellist.ts', 'src/ts/model/types.ts', 'src/ts/storage/database.svelte.ts'],
    })
})
