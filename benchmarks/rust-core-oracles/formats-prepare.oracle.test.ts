import { expect, it, vi } from 'vitest'
import 'fake-indexeddb/auto'
import { readOracleCases, settleOracle, type OracleCase } from './oracleFile'

vi.mock('svelte/internal/client', async (original) => ({
    ...(await original<typeof import('svelte/internal/client')>()), effect_root: () => () => {},
}))
// Fresh record IDs come from the random source; a counter per case keeps the expected files stable.
const ids = vi.hoisted(() => ({ next: 0 }))
vi.mock('uuid', async (original) => {
    const actual = await original<typeof import('uuid')>()
    return { ...actual, v4: () => `00000000-0000-4000-8000-${String(++ids.next).padStart(12, '0')}` }
})
// The platform is a process boundary: `isTauri` reads the WebView's Tauri globals, so a case can ask for the Tauri value.
const platform = vi.hoisted(() => ({ tauri: false }))
vi.mock('../../src/ts/platform', async (original) => {
    const actual = await original<typeof import('../../src/ts/platform')>()
    return { ...actual, get isTauri() { return platform.tauri } }
})

import { defaultSdDataFunc, normalizeDatabaseDefaults, type Database } from '../../src/ts/storage/database.svelte'
import { checkNewFormat, prepareDatabaseForPersistence } from '../../src/ts/storage/databasePreparation'
import { defaultJailbreak, defaultMainPrompt, oldJailbreak, oldMainPrompt } from '../../src/ts/storage/defaultPrompts'
import { prebuiltNAIpresets } from '../../src/ts/process/templates/templates'
import { defaultHotkeys } from '../../src/ts/defaulthotkeys'
import { createHypaV3Preset } from '../../src/ts/process/memory/hypav3'
import { defaultInlayEncodeOptions, MAX_INLAY_ANIMATION_FPS, MAX_INLAY_DIMENSION } from '../../src/ts/storage/blobStore'
import { DEFAULT_CHAT_LOAD_ADDITIONAL_PAGES, DEFAULT_CHAT_LOAD_INITIAL_PAGES } from '../../src/ts/chatLoadPages'
import { alertStore } from '../../src/ts/alertQueue'

// The dialogs are the UI boundary, answered where the app shows them: a confirmation takes the case's next answer
// ('no' once they run out), and any other dialog is dismissed. Each case records the dialog types it saw.
const dialogs = { answers: [] as string[], shown: [] as string[] }
alertStore.subscribe((shown) => {
    if (shown.type === 'none') return
    dialogs.shown.push(shown.type)
    const answer = shown.type === 'ask' ? dialogs.answers.shift() ?? 'no' : ''
    queueMicrotask(() => alertStore.set({ type: 'none', msg: answer }))
})

type PrepareCase = OracleCase & { decline?: string; tauri?: boolean; oldPrompt?: 'main' | 'jailbreak'; answers?: string[] } & (
    | { kind: 'defaults-constants' }
    | { kind: 'defaults'; database: unknown }
    | { kind: 'new-format' | 'persistence'; database: unknown; now: number }
)

// The case file comes from formats-prepare.cases.mjs and drives the real preparation steps.
const settle = {
    generator: import.meta.url,
    tsSources: [
        'src/ts/storage/database.svelte.ts', 'src/ts/storage/databasePreparation.ts', 'src/ts/translator/presets.ts',
        'src/ts/storage/saveCoordinatorHelpers.ts', 'src/ts/characters.ts',
    ],
}
const set = readOracleCases<PrepareCase>('formats', 'prepare')
const message = (error: unknown) => error instanceof Error ? error.message : String(error)

// The values the port reads: the database an empty save normalizes to (every inline default, in step order), the
// constants normalization copies or merges, and the character defaults and prompts `checkNewFormat` uses.
async function constants() {
    const { characters: [character] } = await checkNewFormat({ characters: [{}], formatversion: 5 } as unknown as Database)
    return {
        database: normalizeDatabaseDefaults({} as Database),
        naiSettings: prebuiltNAIpresets,
        hotkeys: defaultHotkeys,
        hypaV3Settings: createHypaV3Preset().settings,
        inlay: { defaults: defaultInlayEncodeOptions, maxDimension: MAX_INLAY_DIMENSION, maxFps: MAX_INLAY_ANIMATION_FPS },
        chatLoadPages: { initial: DEFAULT_CHAT_LOAD_INITIAL_PAGES, additional: DEFAULT_CHAT_LOAD_ADDITIONAL_PAGES },
        character,
        sdData: defaultSdDataFunc(),
        prompts: { oldMainPrompt, oldJailbreak, defaultMainPrompt, defaultJailbreak },
    }
}

// The case's database, with `mainPrompt` set to the old default prompt `oldPrompt` names.
function input(entry: PrepareCase & { database: unknown }): Database {
    const database = structuredClone(entry.database) as Database
    if (entry.oldPrompt) database.mainPrompt = entry.oldPrompt === 'main' ? oldMainPrompt : oldJailbreak
    return database
}

async function outcome(entry: PrepareCase): Promise<string> {
    switch (entry.kind) {
        case 'defaults-constants': return JSON.stringify(await constants())
        case 'defaults': return JSON.stringify(normalizeDatabaseDefaults(input(entry)))
        case 'new-format': return JSON.stringify(await checkNewFormat(input(entry), { now: entry.now }))
        case 'persistence': {
            // The runtime calls it with no options (`persistentDataRuntime.svelte.ts`), so the clock is `Date.now()`.
            const clock = vi.spyOn(Date, 'now').mockReturnValue(entry.now)
            try {
                return JSON.stringify(await prepareDatabaseForPersistence(input(entry)))
            } finally {
                clock.mockRestore()
            }
        }
    }
}

async function run(entry: PrepareCase): Promise<unknown> {
    ids.next = 0
    platform.tauri = entry.tauri === true
    dialogs.answers = [...entry.answers ?? []]
    dialogs.shown = []
    try {
        const ok = await outcome(entry)
        return dialogs.shown.length > 0 ? { ok, dialogs: dialogs.shown } : { ok }
    } catch (error) {
        return { error: message(error) }
    } finally {
        platform.tauri = false
    }
}

it('prepares the database cases', async () => {
    const results: unknown[] = []
    for (const entry of set.cases) results.push(await run(entry))
    const outcomes = results as { ok?: unknown; error?: string }[]
    expect(outcomes.filter(result => typeof result.ok === 'string').length).toBeGreaterThan(50)
    // TS raises no error on purpose in these steps, so every case the port does not decline produced a database.
    set.cases.forEach((entry, index) => {
        if (!entry.decline) expect(outcomes[index], entry.id).toHaveProperty('ok')
    })
    expect(settleOracle(set, results, settle)).toMatch(/written|matched/)
})
