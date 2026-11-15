import { readFileSync } from 'node:fs'
import { createRequire } from 'node:module'
import { afterEach, expect, it, vi } from 'vitest'
import hljs from 'highlight.js/lib/core'
import katex from 'katex'
import DOMPurify from 'dompurify'
import css from '@adobe/css-tools'
import { readOracleCases, settleOracle, type OracleCase } from './oracleFile'

// The display path runs for real against a synthetic database: the parser, the editdisplay scripts, highlight.js, KaTeX,
// DOMPurify, the CSS scoping and the language file. Replaced are the platform flag, the file lookups (`getFileSrc`,
// `getFileImageSource`) and the inlay metadata read, which reach storage. The asset index lookups and
// `processScriptFull` pass through recorders that keep what they were given and returned: in the Rust pipeline both are
// host answers. The app installs the `buffer` 6 package as `globalThis.Buffer` (`src/ts/polyfill.ts`), and the cases
// must run with it, not with Node's, which decodes malformed hex and UTF-8 differently.
vi.unmock('katex')
const PolyfillBuffer = (createRequire(import.meta.url)('buffer/') as typeof import('buffer')).Buffer

// Module-level effects read the database, so their roots start once the first synthetic database is set.
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
// Phase 0 is the asset pass before the scripts, 1 the one after, 2 inside `processScriptFull`.
const host = vi.hoisted(() => ({
    runtime: 'web' as 'web' | 'tauri',
    phase: 0,
    lookups: [] as [number, string, string, AssetFound | null][],
    files: [] as [number, string, string][],
    images: [] as [number, string, string | null][],
    scripts: [] as [string, string][],
    imagePaths: new Set<string>(),
    inlays: new Map<string, { mime: string; inlayType: string; width?: number; height?: number }>(),
    inlayCalls: [] as string[],
}))
vi.mock('../../src/ts/platform', async () => (await import('../../src/ts/platform.testSupport')).platformModule(() => ({ runtime: host.runtime, os: 'windows' })))
vi.mock('../../src/ts/parser/assetLookup', async (original) => {
    const actual = await original<typeof import('../../src/ts/parser/assetLookup')>()
    const copy = (found: { srcPaths: string[]; ext?: string } | null): AssetFound | null => found && { srcPaths: [...found.srcPaths], ext: found.ext ?? null }
    const resolveAdditionalAsset: typeof actual.resolveAdditionalAsset = (index, name, ...rest) => {
        const found = actual.resolveAdditionalAsset(index, name, ...rest)
        host.lookups.push([host.phase, 'additional', name, copy(found)])
        return found
    }
    const resolveEmotionAsset: typeof actual.resolveEmotionAsset = (index, name) => {
        const found = actual.resolveEmotionAsset(index, name)
        host.lookups.push([host.phase, 'emotion', name, copy(found)])
        return found
    }
    return { ...actual, resolveAdditionalAsset, resolveEmotionAsset }
})
vi.mock('../../src/ts/process/scripts', async (original) => {
    const actual = await original<typeof import('../../src/ts/process/scripts')>()
    const processScriptFull: typeof actual.processScriptFull = async (...args) => {
        host.phase = 2
        const result = await actual.processScriptFull(...args)
        host.phase = 1
        host.scripts.push([args[1], result.data])
        return result
    }
    return { ...actual, processScriptFull }
})
vi.mock('../../src/ts/globalApi.svelte', async (original) => {
    const actual = await original<typeof import('../../src/ts/globalApi.svelte')>()
    const getFileSrc: typeof actual.getFileSrc = async (path) => {
        const url = path === 'assets/missing-url.png' ? '' : `risu://${path}`
        host.files.push([host.phase, path, url])
        return url
    }
    const getFileImageSource: typeof actual.getFileImageSource = async (path) => {
        const url = host.imagePaths.has(path) ? `blob:image/${path}` : null
        host.images.push([host.phase, path, url])
        return url === null ? null : {
            url, contentHash: 'synthetic', width: 4, height: 3, recordDimensions: async () => undefined,
            metadata: { key: path, kind: 'asset', size: 1, mime: 'image/png', name: 'synthetic', ext: 'png' },
        }
    }
    return { ...actual, getFileSrc, getFileImageSource }
})
vi.mock('../../src/ts/process/files/inlays', async (original) => ({
    ...await original<typeof import('../../src/ts/process/files/inlays')>(),
    getInlayAssetMetadata: async (id: string) => {
        host.inlayCalls.push(id)
        const inlay = host.inlays.get(id)
        return inlay ? { key: id, kind: 'inlay', size: 1, name: 'synthetic', ext: 'bin', ...inlay } : null
    },
}))
// On Tauri the inlays of a render are read in one call, with every id in match order.
vi.mock('../../src/ts/process/files/inlayRenderSource', async (original) => {
    const actual = await original<typeof import('../../src/ts/process/files/inlayRenderSource')>()
    const getInlayRenderSources: typeof actual.getInlayRenderSources = async (ids) => {
        const sources = new Map<string, InlayRenderSource | null>()
        for (const id of ids) {
            host.inlayCalls.push(id)
            const inlay = host.inlays.get(id)
            sources.set(id, inlay ? {
                url: `native:${id}`, mime: inlay.mime, type: inlay.inlayType as InlayRenderSource['type'], name: 'synthetic', size: 1,
                width: inlay.width, height: inlay.height, objectUrl: false,
            } : null)
        }
        return sources
    }
    return { ...actual, getInlayRenderSources }
})

import { normalizeDatabaseDefaults, setDatabase, getDatabase, getCurrentCharacter, type Database, type customscript } from '../../src/ts/storage/database.svelte'
import { characterFormatUpdate } from '../../src/ts/characters'
import { selectedCharID } from '../../src/ts/stores.svelte'
import { getUserIcon } from '../../src/ts/util'
import { ParseMarkdown, parseMarkdownSafe, postTranslationParse, trimMarkdown, type simpleCharacterArgument } from '../../src/ts/parser/parser.svelte'
import { DeferredInlayMarkerRegistry, type InlayRenderSource } from '../../src/ts/process/files/inlayRenderSource'
import type { ProcessScriptCaptureContext } from '../../src/ts/process/scripts'
import { language } from '../../src/lang'
import { hasCbsSyntax } from '../../src/ts/parser/cbsSyntax'

// Strings that are not well-formed travel as their UTF-16 units, since serde_json rejects isolated surrogate escapes.
type Text = string | { u: number[] }
type Inlay = { id: string; type: string; mime: string; width?: number; height?: number }
type Settings = { customQuotes?: boolean; customQuotesData?: [string, string, string, string]; unformatQuotes?: boolean; blockquoteStyling?: boolean }
type Context = {
    assetWidth?: number; hideAllImages?: boolean; characterImageSource?: string; userImageSource?: string; legacyMediaFindings?: boolean
    assetMaxDifference?: number
}
type PipelineCase = OracleCase & {
    kind: 'display' | 'post-translation' | 'markdown-safe' | 'style' | 'hex' | 'lowercase-ascii' | 'cbs-syntax'
    source?: Text
    hex?: Text
    texts?: Text[]
    mode?: 'normal' | 'back' | 'pretranslate' | 'notrim'
    character?: { chaId: string; assets: [string, string, string][]; emotions: [string, string][]; scripts?: { in: string; out: string }[] }
    chatID?: number
    context?: Context
    platform?: 'tauri'
    images?: string[]
    settings?: Settings
    thoughtMode?: 'off' | 'recent' | 'collapsed'
    inlays?: Inlay[]
    reservation?: boolean
    styleHistory?: boolean
    database?: Partial<Database>
    prior?: { source: string; mode: 'normal' | 'back' | 'pretranslate' | 'notrim' }
    selected?: false
}

const set = readOracleCases<PipelineCase>('render', 'pipeline')
const settle = {
    generator: import.meta.url,
    tsSources: ['src/ts/parser/parser.svelte.ts', 'src/ts/parser/assetLookup.ts', 'src/ts/process/scripts.ts', 'src/ts/process/files/inlayRenderSource.ts',
        'src/ts/parser/cbsSyntax.ts'],
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
const returned = <T>(result: { type: string; value: unknown }): T | null => result.type === 'return' ? result.value as T : null
function optionLanguage(options: unknown) {
    if (typeof options !== 'object' || options === null || !('language' in options) || typeof options.language !== 'string') {
        throw new Error('hljs.highlight called without a language option')
    }
    return options.language
}
const byKey = <T>(values: T[]) => [...values].sort((left, right) => JSON.stringify(left) < JSON.stringify(right) ? -1 : 1)
const settingKeys = ['customQuotes', 'customQuotesData', 'unformatQuotes', 'blockquoteStyling'] as const
const quoteSettings = (settings: Settings) => Object.fromEntries(settingKeys.map(key => [key, settings[key] ?? null]))

function prepare(entry: PipelineCase) {
    const draft = {
        chaId: 'oracle-character', name: 'Oracle', type: 'character', image: 'assets/db-char.png', firstMessage: '', alternateGreetings: [],
        customscript: [], emotionImages: [], additionalAssets: [], globalLore: [], triggerscript: [],
        chats: [{ message: [], note: '', name: 'Chat', localLore: [], id: 'oracle-chat', fmIndex: -1 }], chatPage: 0,
    }
    const database = normalizeDatabaseDefaults({ characters: [draft], userIcon: 'assets/db-user.png', ...entry.database } as unknown as Database)
    characterFormatUpdate(database.characters[0])
    setDatabase(database)
    selectedCharID.set(entry.selected === false ? -1 : 0)
    if (!startup.ready) {
        startup.ready = true
        for (const activate of startup.roots.splice(0)) activate()
    }
}

function spies() {
    const math = vi.spyOn(katex, 'renderToString')
    const highlight = vi.spyOn(hljs, 'highlight')
    return {
        math: () => math.mock.calls.map((call, index) => {
            const value = returned<string>(math.mock.results[index])
            return [encodeText(call[0]), value === null ? null : encodeText(value)]
        }),
        highlights: () => highlight.mock.calls.map((call, index) => {
            const value = returned<{ value: string }>(highlight.mock.results[index])
            return [encodeText(call[0]), optionLanguage(call[1]), value === null ? null : encodeText(value.value)]
        }),
    }
}

function toScript(script: { in: string; out: string }): customscript {
    return { comment: '', in: script.in, out: script.out, type: 'editdisplay' } as customscript
}

function resetHostCalls() {
    host.phase = 0
    host.lookups = []
    host.files = []
    host.images = []
    host.scripts = []
    host.inlayCalls = []
}

// The slots `registry` hands out, with whether `applyAssetImageHints` (in `trimMarkdown`, past the Rust pipeline)
// registered them.
function recordSlots(registry: DeferredInlayMarkerRegistry) {
    const slots: { slot: string | undefined; id: string; hint: boolean }[] = []
    const register = DeferredInlayMarkerRegistry.prototype.register
    const applyAssetImageHints = DeferredInlayMarkerRegistry.prototype.applyAssetImageHints
    let hinting = false
    vi.spyOn(DeferredInlayMarkerRegistry.prototype, 'register').mockImplementation(function (this: DeferredInlayMarkerRegistry, id, source) {
        const slot = register.call(this, id, source)
        if (this === registry) slots.push({ slot, id, hint: hinting })
        return slot
    })
    vi.spyOn(DeferredInlayMarkerRegistry.prototype, 'applyAssetImageHints').mockImplementation(function (this: DeferredInlayMarkerRegistry, root) {
        hinting = true
        try {
            applyAssetImageHints.call(this, root)
        } finally {
            hinting = false
        }
    })
    return slots
}

async function renderDisplay(entry: PipelineCase) {
    host.runtime = entry.platform ?? 'web'
    resetHostCalls()
    host.imagePaths = new Set(entry.images ?? [])
    host.inlays = new Map((entry.inlays ?? []).map(inlay => [inlay.id, { mime: inlay.mime, inlayType: inlay.type, width: inlay.width, height: inlay.height }]))
    const history: string[] = []
    const character = entry.character
    const char: simpleCharacterArgument | null = character ? {
        type: 'simple', chaId: character.chaId, additionalAssets: character.assets, emotionImages: character.emotions,
        customscript: (character.scripts ?? []).map(toScript), triggerscript: [],
    } : null
    const deferredInlays = new DeferredInlayMarkerRegistry()
    const slots = recordSlots(deferredInlays)
    const scriptContext = entry.styleHistory ? {
        parserContext: { prepareHistory: async (source: string) => { history.push(source); return undefined } },
    } as unknown as ProcessScriptCaptureContext : undefined
    const context = entry.context ?? {}
    const mode = entry.mode ?? 'normal'
    // `CSS.supports` answers for the image reservation properties. `vi.unstubAllGlobals` would also drop the globals the
    // shared setup stubs, so the previous value is put back by hand.
    const previousCss = Object.getOwnPropertyDescriptor(globalThis, 'CSS')
    const supports = entry.reservation ?? false
    Object.defineProperty(globalThis, 'CSS', { value: { supports: () => supports }, configurable: true, writable: true })
    let firstSlot = 0
    const render = async () => {
        if (entry.prior) {
            await ParseMarkdown(decodeText(entry.prior.source), char, entry.prior.mode, entry.chatID ?? -1, {}, { ...context, deferredInlays })
            resetHostCalls()
            firstSlot = slots.length
        }
        const record = spies()
        const sanitize = vi.spyOn(DOMPurify, 'sanitize')
        let html: string | null = null
        try {
            html = await ParseMarkdown(decodeText(entry.source ?? ''), char, mode, entry.chatID ?? -1, {}, {
                ...context, markdownSettings: entry.settings, streamingThoughtMode: entry.thoughtMode ?? 'off', deferredInlays, scriptContext,
            })
        } catch (error) {
            // Without a selected character the app throws on a `source` tag; any other throw is the harness's.
            if (entry.selected !== false) throw error
        }
        return { html, record, sanitize }
    }
    let rendered: Awaited<ReturnType<typeof render>>
    try {
        rendered = await render()
    } finally {
        if (previousCss) Object.defineProperty(globalThis, 'CSS', previousCss)
        else delete (globalThis as { CSS?: unknown }).CSS
    }
    const { html: returnedHtml, record, sanitize } = rendered
    const trimInputs = sanitize.mock.calls.map(call => call[0] as string)
    expect(trimInputs.length, entry.id).toBeLessThanOrEqual(mode === 'notrim' ? 0 : 1)
    expect(slots.slice(0, firstSlot).map(({ slot }) => slot), entry.id).toEqual(slots.slice(0, firstSlot).map((_, index) => index.toString(36)))
    const registrations = slots.slice(firstSlot).filter(({ hint }) => !hint)
        .map(({ slot, id }): [number, Text] => [Number.parseInt(slot ?? '', 36), encodeText(id)])
    const database = getDatabase()
    const contextSources = context.characterImageSource !== undefined
    const characterImage = contextSources ? context.characterImageSource : getCurrentCharacter()?.image
    const userImage = contextSources ? context.userImageSource : getUserIcon()
    expect(host.lookups.filter(([phase]) => phase === 2), entry.id).toEqual([])
    expect([...host.files, ...host.images].filter(([phase]) => phase === 2), entry.id).toEqual([])
    return {
        output: returnedHtml === null ? null : mode === 'notrim' ? encodeText(returnedHtml) : trimInputs.length ? encodeText(trimInputs[0]) : null,
        ...(returnedHtml === null ? { threw: true } : {}),
        host: {
            assetWidth: context.assetWidth ?? database.assetWidth ?? null,
            hideAllImages: context.hideAllImages ?? database.hideAllImages ?? false,
            characterImage: characterImage == null ? null : encodeText(characterImage),
            userImage: userImage == null ? null : encodeText(userImage),
            ...(contextSources || getCurrentCharacter() ? {} : { sourcesThrow: true }),
            quotes: quoteSettings(entry.settings ?? database),
        },
        lookups: host.lookups.map(([phase, kind, name, found]) => [phase, kind, encodeText(name), found && {
            srcPaths: found.srcPaths.map(encodeText), ext: found.ext === null ? null : encodeText(found.ext),
        }]),
        files: byKey(host.files.map(([phase, path, url]) => [phase, encodeText(path), encodeText(url)])),
        images: byKey(host.images.map(([phase, path, url]) => [phase, encodeText(path), url === null ? null : encodeText(url)])),
        scripts: host.scripts.map(([input, output]) => [encodeText(input), encodeText(output)]),
        inlayCalls: host.inlayCalls.map(encodeText),
        math: record.math(),
        highlights: record.highlights(),
        firstSlot,
        registrations,
        ...(entry.styleHistory ? { styleHistory: history.length > 0 && history.every(source => source === trimInputs[0]) } : {}),
    }
}

async function renderStyle(entry: PipelineCase) {
    const from = vi.spyOn(PolyfillBuffer, 'from')
    const parse = vi.spyOn(css, 'parse')
    const stringify = vi.spyOn(css, 'stringify')
    const output = trimMarkdown(decodeText(entry.source ?? ''), { returnCSSError: true })
    // `Buffer.from(hex, 'hex')` calls, whose encoding argument the spy's overload type leaves out.
    const hexes = from.mock.calls.map((call): readonly unknown[] => call).filter(call => call[1] === 'hex').map(call => String(call[0]))
    expect(hexes.length, entry.id).toBe(parse.mock.calls.length)
    let next = 0
    const elements = hexes.map((hex, index) => {
        const threw = parse.mock.results[index].type === 'throw'
        const stringified = threw ? null : returned<string>(stringify.mock.results[next++])
        return { hex: encodeText(hex), parsed: encodeText(parse.mock.calls[index][0]), stringified: stringified === null ? null : encodeText(stringified) }
    })
    const template = document.createElement('template')
    template.innerHTML = output
    const styles = Array.from(template.content.querySelectorAll('style'), style => encodeText(style.textContent ?? ''))
    return { output: encodeText(output), elements, styles }
}

function lowercaseAscii() {
    const ascii = (text: string) => /^[\x00-\x7f]*$/.test(text)
    const locales: [string, (text: string) => string][] = [
        ['root', text => text.toLowerCase()], ['default', text => text.toLocaleLowerCase()], ['tr', text => text.toLocaleLowerCase('tr')],
        ['az', text => text.toLocaleLowerCase('az')], ['lt', text => text.toLocaleLowerCase('lt')],
    ]
    const found: [number, string, string][] = []
    for (let code = 0; code <= 0x10ffff; code++) {
        const text = String.fromCodePoint(code)
        for (const [locale, lower] of locales) {
            const result = lower(text)
            const plain = code < 0x80 ? String.fromCharCode(code >= 0x41 && code <= 0x5a ? code + 0x20 : code) : null
            if (plain === null ? ascii(result) : result !== plain) found.push([code, locale, result])
        }
    }
    return found
}

afterEach(() => {
    for (const dispose of startup.dispose.splice(0)) dispose()
    vi.restoreAllMocks()
})

it('reads the labels the Rust pipeline takes from the language file', () => {
    const labels = (JSON.parse(readFileSync(set.casesPath, 'utf8')) as { labels: Record<string, string> }).labels
    expect(labels).toEqual({ cot: language.cot, toolCalled: language.toolCalled, error: language.error })
})

it('records the display pipeline, its host answers and the string stages around it', async () => {
    expect(PolyfillBuffer).not.toBe((await import('node:buffer')).Buffer)
    expect(globalThis.Buffer).toBe(PolyfillBuffer)
    const results: unknown[] = []
    for (const entry of set.cases) {
        prepare(entry)
        vi.spyOn(console, 'log').mockImplementation(() => undefined)
        const errors = vi.spyOn(console, 'error').mockImplementation(() => undefined)
        switch (entry.kind) {
            case 'display':
                results.push(await renderDisplay(entry))
                break
            case 'post-translation': {
                const record = spies()
                const output = await postTranslationParse(decodeText(entry.source ?? ''), entry.settings)
                results.push({ output: encodeText(output), quotes: quoteSettings(entry.settings ?? getDatabase()), math: record.math(), highlights: record.highlights() })
                break
            }
            case 'markdown-safe': {
                const record = spies()
                const sanitize = vi.spyOn(DOMPurify, 'sanitize')
                parseMarkdownSafe(decodeText(entry.source ?? ''))
                results.push({ input: encodeText(sanitize.mock.calls[0][0] as string), quotes: quoteSettings(getDatabase()), math: record.math() })
                break
            }
            case 'style':
                results.push(await renderStyle(entry))
                break
            case 'hex':
                results.push(encodeText(PolyfillBuffer.from(decodeText(entry.hex ?? ''), 'hex').toString('utf-8')))
                break
            case 'lowercase-ascii':
                results.push(lowercaseAscii())
                break
            case 'cbs-syntax':
                results.push((entry.texts ?? []).map(text => hasCbsSyntax(decodeText(text))))
                break
        }
        expect(errors.mock.calls.filter(call => call[0] !== 'KaTeX render error:').map(call => String(call[0])), entry.id).toEqual([])
        vi.restoreAllMocks()
    }
    expect(settleOracle(set, results, settle)).toMatch(/written|matched/)
}, 600_000)
