import { readFileSync } from 'node:fs'
import { setImmediate as immediate } from 'node:timers/promises'
import { afterEach, expect, it, vi } from 'vitest'

// Vitest's asynchronous module graph can schedule Svelte effects before cyclic imports initialize, so the module
// roots start once the imports and a synthetic database exist.
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
type RecordedError = { name: string; message: string }
type OrderedEvent =
    | { kind: 'count-enter'; text: string }
    | { kind: 'count-complete'; text: string; tokens: number }
    | { kind: 'count-error'; text: string; error: RecordedError }
    | { kind: 'setter-enter' | 'setter-complete'; name: string; value: string }
    | { kind: 'setter-error'; name: string; error: RecordedError }
    | { kind: 'locale-lower'; input: string; output: string }
    | { kind: 'gate-held' | 'gate-release' }
const observedSetter = vi.hoisted(() => ({ events: null as OrderedEvent[] | null }))
vi.mock('../../src/ts/process/selectedChatVariables', async (original) => {
    const actual = await original<typeof import('../../src/ts/process/selectedChatVariables')>()
    return { ...actual, setSelectedChatVariable: (...args: Parameters<typeof actual.setSelectedChatVariable>) => {
        const [name, value] = args
        observedSetter.events?.push({ kind: 'setter-enter', name, value })
        try {
            const result = actual.setSelectedChatVariable(...args)
            observedSetter.events?.push({ kind: 'setter-complete', name, value })
            return result
        } catch (error) {
            observedSetter.events?.push({ kind: 'setter-error', name,
                error: { name: (error as Error).name, message: (error as Error).message } })
            throw error
        }
    } }
})
import { CCardLib } from '@risuai/ccardlib'
import { readOracleCases, settleOracle, type OracleCase } from './oracleFile'
import { getDatabase, setDatabase, type Database, type loreBook } from '../../src/ts/storage/database.svelte'
import { DBState, selectedCharID } from '../../src/ts/stores.svelte'
import { pluginV2 } from '../../src/ts/plugins/plugins.svelte'
import { getModuleLorebooks } from '../../src/ts/process/modules'
import { loadLoreBookV3PromptFromCompatibilitySnapshot } from '../../src/ts/process/lorebook.svelte'

type Fields = { [key: string]: unknown }
type LoreCase = OracleCase & {
    root?: Fields
    character?: Fields
    chat?: Fields
    others?: Fields[]
    random?: number[]
    orderedObservation?: {
        mode: 'success' | 'count-reject' | 'count-defer'
        targetId: string
        targetText: string
        normalizeTexts: string[]
    }
    // The decline the native selector gives instead of TS's result, when the case is outside what it runs.
    native?: `unsupported-${string}`
}
type Base = { root: Fields; character: Fields; chat: Fields }

const set = readOracleCases<LoreCase>('lorebook', 'selection')
// The base records live beside the cases so the Rust replay merges them the same way.
const base = (JSON.parse(readFileSync(set.casesPath, 'utf8')) as { base: Base }).base

/** The database a case describes: base records shallowly overridden by the case's records. */
function databaseFor(entry: LoreCase): Fields {
    const chat = { ...base.chat, ...entry.chat }
    const character = { ...base.character, ...entry.character, chats: [chat] }
    return structuredClone({ ...base.root, ...entry.root, characters: [character, ...(entry.others ?? [])] })
}

const readFields = ['username', 'loreBookDepth', 'loreBookToken', 'modules', 'enabledModules', 'moduleIntergration', 'personas',
    'templateDefaultVariables', 'aiModel', 'currentPluginProvider', 'useTokenizerCaching']
// What selection reads must be what the case wrote, so the Rust replay can read the case directly.
function selectionView(database: Fields): Fields {
    const view: Fields = {}
    for (const field of readFields) view[field] = database[field]
    view.characters = (database.characters as Fields[]).map(character => ({
        ...character,
        chats: ((character.chats as Fields[] | undefined) ?? []).map(({ isStreaming: _streaming, activeStreamingDisplayOptimizationMode: _mode, ...chat }) => chat),
    }))
    return view
}

const number = (value: unknown) => {
    const converted = Number(value)
    return Object.is(converted, -0) ? '-0' : String(converted)
}

// Every regex key a case can test, against every text it can scan, answered by V8. The Rust replay looks its
// pattern queries up here, so regex fidelity stays with the regex lane while selection control flow is checked.
function regexTable(database: Fields) {
    const character = (database.characters as Fields[])[0]
    const chat = (character.chats as Fields[])[0]
    const lore = [...((character.globalLore as loreBook[]) ?? []), ...((chat.localLore as loreBook[]) ?? []), ...getModuleLorebooks()]
    const keys = new Set<string>()
    for (const entry of lore) {
        if (!entry.useRegex) continue
        for (const field of [entry.key, entry.secondkey]) {
            if (typeof field === 'string') for (const key of field.split(',')) keys.add(key.trim())
        }
        for (const line of String(entry.content).split('\n')) {
            const trimmed = line.trim()
            if (!trimmed.startsWith('@@')) continue
            const space = trimmed.indexOf(' ')
            if (space !== -1) for (const arg of trimmed.slice(space).split(',')) keys.add(arg.trim())
        }
    }
    const texts = [...new Set([
        ...((chat.message as { data: string }[]) ?? []).map(message => message.data),
        ...lore.filter(entry => typeof entry.content === 'string').map(entry => CCardLib.decorator.parse(entry.content, () => {})),
    ])]
    const patterns: { source: string; flags: string; valid: boolean; matched: number[] }[] = []
    for (const key of keys) {
        const parsed = key.match(/^\/([\s\S]*)\/([dgimsuvy]*)$/)
        if (!parsed) continue
        let regex: RegExp
        try {
            regex = new RegExp(parsed[1], parsed[2])
        } catch {
            patterns.push({ source: parsed[1], flags: parsed[2], valid: false, matched: [] })
            continue
        }
        const matched = texts.flatMap((text, index) => {
            regex.lastIndex = 0
            return regex.test(text) ? [index] : []
        })
        patterns.push({ source: parsed[1], flags: parsed[2], valid: true, matched })
    }
    return patterns.length ? { texts, patterns } : undefined
}

// Whether any text selection could lowercase (keys, decorator arguments, lore content, message text and speaker
// names) lowercases differently under a tailored locale. The native selector may decline such a case when told the
// default locale is tailored; it must not decline any other.
function tailoredCase(database: Fields) {
    const characters = database.characters as Fields[]
    const chat = (characters[0].chats as Fields[])[0]
    const lore = [...((characters[0].globalLore as loreBook[]) ?? []), ...((chat.localLore as loreBook[]) ?? []), ...getModuleLorebooks()]
    const messages = (chat.message as { data: unknown; name?: unknown }[]) ?? []
    const texts = [
        ...lore.flatMap(entry => [entry.key, entry.secondkey, entry.content]),
        ...messages.flatMap(message => [message.data, message.name]),
        database.username, ...characters.map(character => character.name),
    ].filter((text): text is string => typeof text === 'string')
    return texts.some(text => ['tr', 'az', 'lt', 'el'].some(locale => text.toLocaleLowerCase(locale) !== text.toLowerCase()))
}

afterEach(() => {
    for (const dispose of startup.dispose.splice(0)) dispose()
    observedSetter.events = null
    vi.restoreAllMocks()
})

it('records real loadLoreBookV3Prompt selections over synthetic databases', async () => {
    // Expectations assume the root case mapping; a tailored default locale would change toLocaleLowerCase.
    expect(['I', '\u0130', '\u00cc'].map(text => text.toLocaleLowerCase())).toEqual(['i', 'i\u0307', '\u00ec'])
    vi.spyOn(console, 'log').mockImplementation(() => {})
    const tokenizer: { text: string; tokens: number }[] = []
    const results: unknown[] = []
    for (const entry of set.cases) {
        const database = databaseFor(entry)
        // Memo cases name their own provider so the tokenizer cache, keyed by provider, starts empty.
        const provider = String(database.currentPluginProvider)
        const ordered = entry.orderedObservation ? { events: [] as OrderedEvent[], heldScriptstate: undefined as Record<string, string> | undefined } : undefined
        let held!: () => void
        let release!: () => void
        const entered = new Promise<void>(resolve => { held = resolve })
        const released = new Promise<void>(resolve => { release = resolve })
        pluginV2.providerOptions.set(provider, {
            tokenizer: 'custom',
            tokenizerFunc: (text: string) => {
                ordered?.events.push({ kind: 'count-enter', text })
                const complete = () => {
                    const tokens = Math.ceil(text.length / 4)
                    tokenizer.push({ text, tokens })
                    ordered?.events.push({ kind: 'count-complete', text, tokens })
                    return new Array<number>(tokens).fill(0)
                }
                if (entry.orderedObservation?.targetText === text) {
                    if (entry.orderedObservation.mode === 'count-reject') {
                        const error = new Error('ordered-current-tokenizer-failure')
                        ordered!.events.push({ kind: 'count-error', text, error: { name: error.name, message: error.message } })
                        throw error
                    }
                    if (entry.orderedObservation.mode === 'count-defer') {
                        held()
                        return released.then(complete)
                    }
                }
                return complete()
            },
        })
        setDatabase(structuredClone(database) as unknown as Database)
        selectedCharID.set(0)
        if (!startup.ready) {
            startup.ready = true
            for (const activate of startup.roots.splice(0)) activate()
        }
        // Let the store effects settle before the tape starts.
        await immediate()
        expect(selectionView(getDatabase({ snapshot: true }) as unknown as Fields), entry.id).toEqual(selectionView(database))
        tokenizer.splice(0)
        const tape = entry.random ?? []
        let drawn = 0
        vi.spyOn(Math, 'random').mockImplementation(() => {
            if (drawn >= tape.length) throw new Error('rng-tape exhausted')
            return tape[drawn++]
        })
        const chat = () => DBState.db.characters[0].chats[0]
        const before = { ...(chat().scriptstate ?? {}) }
        let selection: Awaited<ReturnType<typeof loadLoreBookV3PromptFromCompatibilitySnapshot>> | undefined
        let error: { name: string; message: string } | undefined
        const lower = String.prototype.toLocaleLowerCase
        const normalizeTexts = new Set(entry.orderedObservation?.normalizeTexts ?? [])
        const localeSpy = ordered ? vi.spyOn(String.prototype, 'toLocaleLowerCase').mockImplementation(function(this: string, ...args: Parameters<typeof lower>) {
            const output = lower.apply(this, args)
            const input = String(this)
            if (normalizeTexts.has(input)) ordered.events.push({ kind: 'locale-lower', input, output })
            return output
        }) : undefined
        observedSetter.events = ordered?.events ?? null
        const pending = loadLoreBookV3PromptFromCompatibilitySnapshot()
        const settled = pending.then(
            value => ({ ok: true as const, value }),
            thrown => ({ ok: false as const, thrown }),
        )
        let harnessFailure: { error: unknown } | undefined
        try {
            if (entry.orderedObservation?.mode === 'count-defer') {
                const reachedGate = await Promise.race([entered.then(() => true), settled.then(() => false)])
                if (!reachedGate) throw new Error('Selection completed before deferred count')
                const state = getDatabase({ snapshot: true }).characters[0].chats[0].scriptstate ?? {}
                expect(Object.values(state).every(value => typeof value === 'string'), entry.id).toBe(true)
                ordered!.heldScriptstate = { ...state } as Record<string, string>
                ordered!.events.push({ kind: 'gate-held' }, { kind: 'gate-release' })
            }
        } catch (thrown) {
            harnessFailure = { error: thrown }
        } finally {
            release()
            try {
                const outcome = await settled
                if (outcome.ok === true) selection = outcome.value
                else error = { name: (outcome.thrown as Error).name, message: (outcome.thrown as Error).message }
            } finally {
                observedSetter.events = null
                localeSpy?.mockRestore()
                vi.mocked(Math.random).mockRestore()
                pluginV2.providerOptions.delete(provider)
            }
        }
        if (harnessFailure) throw harnessFailure.error
        const after = getDatabase({ snapshot: true }).characters[0].chats[0].scriptstate ?? {}
        const writes = Object.entries(after).filter(([key, value]) => before[key] !== value).map(([key, value]) => [key.slice(1), value])
        const regex = regexTable(database)
        results.push({
            actives: selection?.actives.map(active => ({
                depth: number(active.depth), pos: active.pos, prompt: active.prompt, role: active.role, order: number(active.order),
                tokens: active.tokens, priority: number(active.priority), source: active.source, inject: active.inject,
            })) ?? null,
            matchLog: selection?.matchLog ?? null,
            writes,
            random: drawn,
            tokenizer: tokenizer.slice(),
            tailoredCase: tailoredCase(database),
            ...(regex ? { regex } : {}),
            ...(error ? { error } : {}),
            ...(ordered ? { orderedObservation: { events: ordered.events,
                ...(ordered.heldScriptstate ? { heldScriptstate: ordered.heldScriptstate } : {}) } } : {}),
        })
    }
    expect(results.length).toBeGreaterThanOrEqual(120)
    const previous = JSON.parse(readFileSync(set.expectedPath, 'utf8')) as { cases: { id: string; result: unknown }[] }
    for (const [index, entry] of set.cases.entries()) {
        if (!entry.orderedObservation) {
            expect(results[index], `Unchanged selection oracle ${entry.id}`).toEqual(previous.cases.find(value => value.id === entry.id)!.result)
        }
    }
    settleOracle(set, results, {
        generator: import.meta.url,
        tsSources: ['src/ts/process/lorebook.svelte.ts', 'src/ts/process/modules.ts', 'src/ts/process/moduleSelection.ts',
            'src/ts/process/selectedChatVariables.ts', 'src/ts/parser/chatVar.svelte.ts', 'src/ts/util.ts', 'src/ts/tokenizer.ts'],
    })
})
