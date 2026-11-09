import { writeFileSync } from 'node:fs'
import { resolve } from 'node:path'
import { afterEach, expect, it, vi } from 'vitest'
import hljs from 'highlight.js/lib/core'
import katex from 'katex'
import { timingCharacter, timingInlays, timingInputs } from './render-timing.inputs.mjs'

// Times the display render the Rust pipeline replaces, `ParseMarkdown(..., 'notrim')` up to `trimMarkdown`, on the
// synthetic messages of render-timing.inputs.mjs, and records what its host answered so the Rust `wiring_cost` test
// can replay the same render. Set RISUNEST_RENDER_TIMING_OUT to the JSON file to write; without it the test is
// skipped. The mock graph is the pipeline oracle's on the web runtime. KaTeX, highlight.js, `processScriptFull`, the
// asset index lookups and the file and inlay reads stay host work under the Rust pipeline, so their time is measured
// separately and reported apart from the string stages.
vi.unmock('katex')

const output = process.env.RISUNEST_RENDER_TIMING_OUT
const warmups = 3
const samples = 15

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

type AssetFound = { srcPaths: string[]; ext: string | null }
type HostTime = { scripts: number; lookups: number; files: number; inlays: number; katex: number; hljs: number }
const host = vi.hoisted(() => ({
    phase: 0,
    lookups: [] as [number, string, string, AssetFound | null][],
    files: [] as [number, string, string][],
    scripts: [] as [string, string][],
    inlayCalls: [] as string[],
    inlays: new Map<string, { mime: string; inlayType: string; width?: number; height?: number }>(),
    time: { scripts: 0, lookups: 0, files: 0, inlays: 0, katex: 0, hljs: 0 } as HostTime,
}))
function timed<T>(part: keyof HostTime, run: () => T): T {
    const started = performance.now()
    try {
        return run()
    } finally {
        host.time[part] += performance.now() - started
    }
}
vi.mock('../../src/ts/platform', async () => (await import('../../src/ts/platform.testSupport')).platformModule(() => ({ runtime: 'web', os: 'windows' })))
vi.mock('../../src/ts/parser/assetLookup', async (original) => {
    const actual = await original<typeof import('../../src/ts/parser/assetLookup')>()
    const copy = (found: { srcPaths: string[]; ext?: string } | null): AssetFound | null => found && { srcPaths: [...found.srcPaths], ext: found.ext ?? null }
    const resolveAdditionalAsset: typeof actual.resolveAdditionalAsset = (index, name, ...rest) => {
        const found = timed('lookups', () => actual.resolveAdditionalAsset(index, name, ...rest))
        host.lookups.push([host.phase, 'additional', name, copy(found)])
        return found
    }
    const resolveEmotionAsset: typeof actual.resolveEmotionAsset = (index, name) => {
        const found = timed('lookups', () => actual.resolveEmotionAsset(index, name))
        host.lookups.push([host.phase, 'emotion', name, copy(found)])
        return found
    }
    return { ...actual, resolveAdditionalAsset, resolveEmotionAsset }
})
vi.mock('../../src/ts/process/scripts', async (original) => {
    const actual = await original<typeof import('../../src/ts/process/scripts')>()
    const processScriptFull: typeof actual.processScriptFull = async (...args) => {
        host.phase = 2
        const started = performance.now()
        const result = await actual.processScriptFull(...args)
        host.time.scripts += performance.now() - started
        host.phase = 1
        host.scripts.push([args[1], result.data])
        return result
    }
    return { ...actual, processScriptFull }
})
vi.mock('../../src/ts/globalApi.svelte', async (original) => {
    const actual = await original<typeof import('../../src/ts/globalApi.svelte')>()
    const getFileSrc: typeof actual.getFileSrc = async (path) => timed('files', () => {
        const url = `risu://${path}`
        host.files.push([host.phase, path, url])
        return url
    })
    return { ...actual, getFileSrc }
})
vi.mock('../../src/ts/process/files/inlays', async (original) => ({
    ...await original<typeof import('../../src/ts/process/files/inlays')>(),
    getInlayAssetMetadata: async (id: string) => timed('inlays', () => {
        host.inlayCalls.push(id)
        const inlay = host.inlays.get(id)
        return inlay ? { key: id, kind: 'inlay', size: 1, name: 'synthetic', ext: 'bin', ...inlay } : null
    }),
}))

import { normalizeDatabaseDefaults, setDatabase, getDatabase, type Database, type customscript } from '../../src/ts/storage/database.svelte'
import { characterFormatUpdate } from '../../src/ts/characters'
import { selectedCharID } from '../../src/ts/stores.svelte'
import { ParseMarkdown, type simpleCharacterArgument } from '../../src/ts/parser/parser.svelte'
import { DeferredInlayMarkerRegistry } from '../../src/ts/process/files/inlayRenderSource'
import { language } from '../../src/lang'

const context = { assetWidth: 2, characterImageSource: 'assets/char.png', userImageSource: 'assets/user.png' }

function prepare() {
    const draft = {
        chaId: 'timing-database-character', name: 'Timing', type: 'character', image: 'assets/db-char.png', firstMessage: '', alternateGreetings: [],
        customscript: [], emotionImages: [], additionalAssets: [], globalLore: [], triggerscript: [],
        chats: [{ message: [], note: '', name: 'Chat', localLore: [], id: 'timing-chat', fmIndex: -1 }], chatPage: 0,
    }
    const database = normalizeDatabaseDefaults({ characters: [draft], userIcon: 'assets/db-user.png' } as unknown as Database)
    characterFormatUpdate(database.characters[0])
    setDatabase(database)
    selectedCharID.set(0)
    if (!startup.ready) {
        startup.ready = true
        for (const activate of startup.roots.splice(0)) activate()
    }
}

function resetHost() {
    host.phase = 0
    host.lookups = []
    host.files = []
    host.scripts = []
    host.inlayCalls = []
    host.time = { scripts: 0, lookups: 0, files: 0, inlays: 0, katex: 0, hljs: 0 }
}

function median(values: number[]) {
    const sorted = [...values].sort((left, right) => left - right)
    return sorted[Math.floor(sorted.length / 2)]
}

afterEach(() => {
    for (const dispose of startup.dispose.splice(0)) dispose()
    vi.restoreAllMocks()
})

it.skipIf(!output)('times the display render on synthetic messages and records its host answers', async () => {
    prepare()
    vi.spyOn(console, 'log').mockImplementation(() => undefined)
    host.inlays = new Map(timingInlays.map(inlay => [inlay.id, { mime: inlay.mime, inlayType: inlay.type, width: inlay.width, height: inlay.height }]))
    const renderToString = katex.renderToString
    const highlight = hljs.highlight
    const math = vi.spyOn(katex, 'renderToString').mockImplementation((...args) => timed('katex', () => renderToString(...args)))
    const highlights = vi.spyOn(hljs, 'highlight').mockImplementation((...args) => timed('hljs', () => highlight(...args)))
    const results: unknown[] = []
    for (const input of timingInputs()) {
        const char: simpleCharacterArgument = {
            type: 'simple', chaId: timingCharacter.chaId, additionalAssets: timingCharacter.assets as [string, string, string][],
            emotionImages: timingCharacter.emotions as [string, string][],
            customscript: input.scripts.map(script => ({ comment: '', in: script.in, out: script.out, type: 'editdisplay' } as customscript)),
            triggerscript: [],
        }
        const render = () => ParseMarkdown(input.source, char, 'notrim', 0, {}, { ...context, deferredInlays: new DeferredInlayMarkerRegistry() })
        resetHost()
        math.mockClear()
        highlights.mockClear()
        const html = await render()
        const record = {
            output: html,
            lookups: [...host.lookups],
            files: [...host.files],
            scripts: [...host.scripts],
            inlayCalls: [...host.inlayCalls],
            math: math.mock.calls.map((call, index) => [call[0], math.mock.results[index].type === 'return' ? math.mock.results[index].value : null]),
            highlights: highlights.mock.calls.map((call, index) => {
                const result = highlights.mock.results[index]
                const options = call[1] as { language?: unknown } | string | undefined
                const language = typeof options === 'object' ? String(options.language ?? '') : ''
                return [call[0], language, result.type === 'return' ? result.value.value : null]
            }),
        }
        for (let index = 0; index < warmups; index++) await render()
        const totals: number[] = []
        const stringPath: number[] = []
        const hostParts: HostTime[] = []
        for (let index = 0; index < samples; index++) {
            resetHost()
            const started = performance.now()
            const rendered = await render()
            const total = performance.now() - started
            expect(rendered).toBe(html)
            const parts = { ...host.time }
            totals.push(total)
            hostParts.push(parts)
            stringPath.push(total - Object.values(parts).reduce((sum, value) => sum + value, 0))
        }
        const hostMedian = Object.fromEntries((Object.keys(host.time) as (keyof HostTime)[]).map(part => [part, median(hostParts.map(parts => parts[part]))]))
        results.push({
            id: input.id, length: input.source.length, source: input.source, scripts: input.scripts, record,
            ts: { totalMs: median(totals), stringPathMs: median(stringPath), hostMs: hostMedian, samples: totals },
        })
    }
    const database = getDatabase()
    const file = {
        schema: 1,
        node: process.version,
        labels: { cot: language.cot, toolCalled: language.toolCalled, error: language.error },
        host: {
            assetWidth: context.assetWidth, hideAllImages: database.hideAllImages ?? false, characterImage: context.characterImageSource,
            userImage: context.userImageSource, chatID: 0, chaId: timingCharacter.chaId,
            // What the image reservation reads; the harness DOM supports both properties, as current WebViews do.
            reservation: typeof CSS !== 'undefined' && CSS.supports('contain', 'size') && CSS.supports('contain-intrinsic-size', '1px 1px'),
            quotes: { customQuotes: database.customQuotes ?? null, customQuotesData: database.customQuotesData ?? null,
                unformatQuotes: database.unformatQuotes ?? null, blockquoteStyling: database.blockquoteStyling ?? null },
        },
        inlays: timingInlays,
        warmups, samples,
        inputs: results,
    }
    writeFileSync(resolve(output ?? ''), `${JSON.stringify(file)}\n`)
}, 900_000)
