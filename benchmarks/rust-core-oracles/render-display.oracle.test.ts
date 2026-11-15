import { readFileSync } from 'node:fs'
import { afterEach, expect, it, vi } from 'vitest'
import { writable } from 'svelte/store'
import hljs from 'highlight.js/lib/core'
import katex from 'katex'
import { readOracleCases, settleOracle, type OracleCase } from './oracleFile'

// The display path runs for real: the parser module, highlight.js, KaTeX and the language file. Only what reads app
// state or storage is replaced: the database and stores, the platform, and the inlay metadata lookup.
vi.unmock('katex')
const stored = vi.hoisted(() => ({ inlays: new Map<string, unknown>() }))
vi.mock('../../src/ts/platform', async () => (await import('../../src/ts/platform.testSupport')).platformModule({ runtime: 'web', os: 'windows' }))
vi.mock('../../src/ts/storage/database.svelte', () => ({
    appVer: 'synthetic',
    getCurrentCharacter: () => ({}),
    getDatabase: () => ({}),
}))
vi.mock('../../src/ts/globalApi.svelte', () => ({
    aiWatermarkingLawApplies: () => false,
    getFileSrc: async () => '',
    getFileImageSource: async () => null,
}))
vi.mock('../../src/ts/stores.svelte', () => ({
    DBState: { db: { characters: [], globalChatVariables: {}, templateDefaultVariables: '', hideAllImages: false } },
    selIdState: { selId: -1 },
    selectedCharID: writable(-1),
}))
vi.mock('../../src/ts/process/files/inlays', async (original) => ({
    ...await original<typeof import('../../src/ts/process/files/inlays')>(),
    getInlayAssetMetadata: async (id: string) => stored.inlays.get(id) ?? null,
}))

import { ParseMarkdown } from '../../src/ts/parser/parser.svelte'
import { DeferredInlayMarkerRegistry } from '../../src/ts/process/files/inlayRenderSource'
import { language } from '../../src/lang'

// Strings that are not well-formed travel as their UTF-16 units, since serde_json rejects isolated surrogate escapes.
type Text = string | { u: number[] }
type Inlay = { id: string; type: string; mime: string; width?: number; height?: number }
type DisplayCase = OracleCase & {
    source: Text
    settings?: Record<string, unknown>
    thoughtMode?: 'off' | 'recent' | 'collapsed'
    hideAllImages?: boolean
    reservation?: boolean
    inlays?: Inlay[]
    registry?: 'disposed'
    segmenter?: 'none' | 'before16' | 'unicode17' | 'from18'
    decline?: string
}
type Result = {
    html: Text
    // What KaTeX and highlight.js were given and gave back, in call order; `null` where the call threw.
    math: [Text, Text | null][]
    highlights: [Text, string, Text | null][]
    // The registry's markers by slot.
    registrations: [number, Text][]
}

const set = readOracleCases<DisplayCase>('render', 'display')
const labels = (JSON.parse(readFileSync(set.casesPath, 'utf8')) as { labels: Record<string, string> }).labels
const settle = {
    generator: import.meta.url,
    tsSources: ['src/ts/parser/parser.svelte.ts', 'src/ts/parser/streamingThoughtPreview.ts', 'src/ts/process/caseUnicode.ts',
        'src/ts/process/files/inlayRenderSource.ts', 'src/ts/process/files/imageGeometryRender.ts'],
}

function decodeText(text: Text) {
    if (typeof text === 'string') return text
    let value = ''
    for (const unit of text.u) value += String.fromCharCode(unit)
    return value
}
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
const encodeText = (text: string): Text => wellFormed(text) ? text : { u: Array.from({ length: text.length }, (_, index) => text.charCodeAt(index)) }

// What a spied call returned, or `null` where it threw.
const returned = <T>(result: { type: string; value: unknown }): T | null => result.type === 'return' ? result.value as T : null
function optionLanguage(options: unknown) {
    if (typeof options !== 'object' || options === null || !('language' in options) || typeof options.language !== 'string') {
        throw new Error('hljs.highlight called without a language option')
    }
    return options.language
}

// `ParseMarkdown` from a fresh module graph loaded while `Intl.Segmenter` is missing, as in a WebView without it:
// `streamingThoughtPreview.ts` picks its segmenter when it loads. The graph's own KaTeX and highlight.js are the ones
// its parser calls, so the spies go on those.
type Graph = { parse: typeof ParseMarkdown; katex: typeof katex; hljs: typeof hljs }
const loadedGraph: Graph = { parse: ParseMarkdown, katex, hljs }
let graphWithoutSegmenter: Graph | undefined
async function withoutSegmenter() {
    if (graphWithoutSegmenter) return graphWithoutSegmenter
    const segmenter = Intl.Segmenter
    vi.resetModules()
    Object.defineProperty(Intl, 'Segmenter', { value: undefined })
    try {
        const parser = await import('../../src/ts/parser/parser.svelte')
        graphWithoutSegmenter = { parse: parser.ParseMarkdown, katex: (await import('katex')).default, hljs: (await import('highlight.js/lib/core')).default }
    } finally {
        Object.defineProperty(Intl, 'Segmenter', { value: segmenter })
    }
    return graphWithoutSegmenter
}

afterEach(() => {
    vi.restoreAllMocks()
    vi.unstubAllGlobals()
})

it('reads the labels the Rust pipeline takes from the language file', () => {
    expect(labels).toEqual({ cot: language.cot, toolCalled: language.toolCalled, error: language.error })
})

it('renders the display cases', async () => {
    const results: Result[] = []
    let segmenterMatters = false
    for (const entry of set.cases) {
        stored.inlays = new Map((entry.inlays ?? []).map(inlay => [inlay.id, {
            key: inlay.id, kind: 'inlay', size: 1, mime: inlay.mime, name: 'synthetic', ext: 'bin', inlayType: inlay.type,
            width: inlay.width, height: inlay.height,
        }]))
        const supports = entry.reservation ?? false
        vi.stubGlobal('CSS', { supports: () => supports })
        const graph = entry.segmenter === 'none' ? await withoutSegmenter() : loadedGraph
        const renderMath = vi.spyOn(graph.katex, 'renderToString')
        const highlight = vi.spyOn(graph.hljs, 'highlight')
        const error = vi.spyOn(console, 'error').mockImplementation(() => undefined)
        const deferredInlays = new DeferredInlayMarkerRegistry()
        if (entry.registry === 'disposed') deferredInlays.clear()
        const context = (deferredInlays: DeferredInlayMarkerRegistry) => ({
            markdownSettings: entry.settings ?? {},
            streamingThoughtMode: entry.thoughtMode ?? 'off',
            hideAllImages: entry.hideAllImages ?? false,
            deferredInlays,
        })
        const html = await graph.parse(decodeText(entry.source), null, 'notrim', -1, {}, context(deferredInlays))
        const registrations: [number, Text][] = []
        for (let slot = 0; ; slot++) {
            const marker = deferredInlays.seal(slot.toString(36))
            if (!marker) break
            registrations.push([slot, encodeText(marker.id)])
        }
        results.push({
            html: encodeText(html),
            math: renderMath.mock.calls.map((call, index) => {
                const value = returned<string>(renderMath.mock.results[index])
                return [encodeText(call[0]), value === null ? null : encodeText(value)]
            }),
            highlights: highlight.mock.calls.map((call, index) => {
                const value = returned<{ value: string }>(highlight.mock.results[index])
                return [encodeText(call[0]), optionLanguage(call[1]), value === null ? null : encodeText(value.value)]
            }),
            registrations,
        })
        if (entry.segmenter === 'none') {
            const withSegmenter = await ParseMarkdown(decodeText(entry.source), null, 'notrim', -1, {}, context(new DeferredInlayMarkerRegistry()))
            segmenterMatters ||= withSegmenter !== html
        }
        expect(error.mock.calls.every(call => call[0] === 'KaTeX render error:'), entry.id).toBe(true)
        vi.restoreAllMocks()
        vi.unstubAllGlobals()
    }
    expect(results.some(result => result.math.some(([, value]) => value === null))).toBe(true)
    expect(results.some(result => result.highlights.length > 0)).toBe(true)
    expect(results.some(result => result.registrations.length > 1)).toBe(true)
    expect(segmenterMatters).toBe(true)
    expect(settleOracle(set, results, settle)).toMatch(/written|matched/)
})

type GraphemeCase = OracleCase & {
    seconds?: number[]
    simple?: [number, number][]
    classLeads?: number[]
    case16Small?: number
    case16Capital?: number
    case17?: number
    segment16?: number
    segment17?: number
}

function required<T>(value: T | undefined, name: string): T {
    if (value === undefined) throw new Error(`grapheme case without ${name}`)
    return value
}

it('records where Intl.Segmenter ends the first cluster, and the engine probes', () => {
    const grapheme = readOracleCases<GraphemeCase>('render', 'grapheme')
    const segmenter = new Intl.Segmenter(undefined, { granularity: 'grapheme' })
    const first = (text: string) => segmenter.segment(text)[Symbol.iterator]().next().value?.segment ?? ''
    // A surrogate code point stays one lone unit.
    const char = (code: number) => String.fromCodePoint(code)
    const joins = (lead: number, second: number) => first(char(lead) + char(second)).length > char(lead).length
    const ranges = (test: (code: number) => boolean) => {
        const found: [number, number][] = []
        for (let code = 0; code <= 0x10ffff; code++) {
            if (!test(code)) continue
            const last = found.at(-1)
            if (last && last[1] === code - 1) last[1] = code
            else found.push([code, code])
        }
        return found
    }
    // The stand-in with the same Grapheme_Cluster_Break value as `code`, among the seconds the Rust rules accept.
    const standIn = (code: number) => code === 0x0a || code === 0x0d ? code
        : code < 0x20 || code === 0x7f ? 0x09
        : code >= 0xac00 && code <= 0xd7a3 ? ((code - 0xac00) % 28 === 0 ? 0xac00 : 0xac01)
        : 0x61
    const results = grapheme.cases.map(entry => {
        switch (entry.id) {
            case 'first-cluster': {
                const classMismatches: [number, number][] = []
                for (const lead of required(entry.classLeads, 'classLeads')) {
                    for (const [start, end] of required(entry.simple, 'simple')) {
                        for (let second = start; second <= end; second++) {
                            if (joins(lead, second) !== joins(lead, standIn(second))) classMismatches.push([lead, second])
                        }
                    }
                }
                return { joined: required(entry.seconds, 'seconds').map(second => ranges(lead => joins(lead, second))), classMismatches }
            }
            case 'extend-after-a':
                return ranges(code => first('a' + char(code)).length > 1)
            case 'probes': {
                const [small, capital, case17] = [required(entry.case16Small, 'case16Small'), required(entry.case16Capital, 'case16Capital'), required(entry.case17, 'case17')]
                const [segment16, segment17] = [required(entry.segment16, 'segment16'), required(entry.segment17, 'segment17')]
                return {
                    case16: char(small).toUpperCase() === char(capital),
                    case17: char(case17).toUpperCase() !== char(case17),
                    segment16: first(char(segment16) + 'a') === char(segment16) + 'a',
                    segment17: first('a' + char(segment17)) === 'a' + char(segment17),
                }
            }
            default:
                throw new Error(`unknown grapheme case ${entry.id}`)
        }
    })
    expect(results[0]).toMatchObject({ classMismatches: [] })
    expect(results[2]).toEqual({ case16: true, case17: false, segment16: true, segment17: false })
    expect(settleOracle(grapheme, results, { generator: import.meta.url,
        tsSources: ['src/ts/parser/streamingThoughtPreview.ts', 'src/ts/process/caseUnicode.ts'] })).toMatch(/written|matched/)
}, 600_000)
