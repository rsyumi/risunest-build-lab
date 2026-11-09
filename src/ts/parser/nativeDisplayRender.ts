import { invoke } from '@tauri-apps/api/core'
import type { LanguageFn } from 'highlight.js'
import hljs from 'highlight.js/lib/core'
import katex from 'katex'
import { caseUnicode } from '../process/caseUnicode'
import type { DeferredInlayMarkerRegistry, InlayRenderSource } from '../process/files/inlayRenderSource'
import { graphemeSegmenterUnicode, type UnicodeVersion } from './streamingThoughtPreview'

/** A string as the native render takes and returns it: UTF-16 units when it holds an isolated surrogate. */
type WireText = string | { u: number[] }

const isolatedSurrogate = /[\uD800-\uDBFF](?![\uDC00-\uDFFF])|(?<![\uD800-\uDBFF])[\uDC00-\uDFFF]/

function wire(value: string): WireText {
    if (!isolatedSurrogate.test(value)) return value
    const u = new Array<number>(value.length)
    for (let index = 0; index < value.length; index++) u[index] = value.charCodeAt(index)
    return { u }
}

function read(value: WireText): string {
    if (typeof value === 'string') return value
    let text = ''
    for (let index = 0; index < value.u.length; index += 8192) text += String.fromCharCode(...value.u.slice(index, index + 8192))
    return text
}

function wireOrNull(value: string | null | undefined): WireText | null {
    return value === null || value === undefined ? null : wire(value)
}

export type NativeDisplayMode = 'normal' | 'back' | 'pretranslate' | 'notrim'

/** What `parseMarkdownForDisplay` reads for one render, before the host answers anything. */
export interface NativeDisplayRequest {
    text: string
    mode: NativeDisplayMode
    character: {
        assetWidth: number | null
        chatId: number
        chaId: string
        characterImage: string | null
        userImage: string | null
        /** No character is selected and the render context holds no image source, so TS throws on a `source` tag. */
        sourcesThrow: boolean
    } | null
    hideAllImages: boolean
    imageReservation: boolean
    thoughtMode: 'off' | 'recent' | 'collapsed'
    labels: { cot: string, toolCalled: string, error: string }
    quotes: {
        customQuotes: boolean
        customQuotesData: readonly string[] | null
        unformatQuotes: boolean
        blockquoteStyling: boolean
    }
}

/** The app's answers to the render's requests. */
export interface NativeDisplayHost {
    /** `resolveEmotionAsset` for `emotion`, `resolveAdditionalAsset` for the other tags. */
    lookup(tag: string, name: string): { srcPaths: string[], ext?: string } | null | undefined
    /** The URL of an asset path; for an `image` request, the image source when there is one. */
    url(path: string, image: boolean): Promise<string>
    /** The `editdisplay` scripts on `text`. */
    scripts(text: string): Promise<string>
    inlays(ids: string[]): Promise<ReadonlyMap<string, InlayRenderSource | null>>
    registry: DeferredInlayMarkerRegistry
    signal?: AbortSignal
}

export type NativeDisplayResult =
    | { kind: 'done', html: string, trim: boolean, styleHistory: boolean }
    /** The page renders the message in TS, from `scripted` when the scripts already ran. */
    | { kind: 'declined', reason: string, scripted: { firstParsed: string, data: string } | null }

type Step =
    | { kind: 'assetLookups', pass: number, lookups: { tag: string, name: WireText }[] }
    | { kind: 'assetUrls', pass: number, requests: { path: WireText, image: boolean }[] }
    | { kind: 'scripts', text: WireText }
    | { kind: 'inlays', ids: WireText[] }
    | { kind: 'math', tex: WireText[] }
    | { kind: 'highlights', blocks: { language: string, code: WireText }[] }
    | { kind: 'done', html: WireText, registrations: { slot: number, id: WireText }[], trim: boolean, styleHistory: boolean }

type Response = { session: number | null, step: Step } | { declined: string } | { failed: string }

let engine: { caseUnicode: UnicodeVersion, segmenter: UnicodeVersion | null } | undefined

/** The Unicode tables of this WebView's case mapping and of the thought preview's segmenter. */
function jsEngine() {
    return engine ??= { caseUnicode: caseUnicode(), segmenter: graphemeSegmenterUnicode() }
}

// The languages renderHighlightableMarkdown loads, under the names it registers them by.
const highlightLanguages: Record<string, () => Promise<{ default: LanguageFn }>> = {
    bash: () => import('highlight.js/lib/languages/bash'),
    cpp: () => import('highlight.js/lib/languages/cpp'),
    csharp: () => import('highlight.js/lib/languages/csharp'),
    css: () => import('highlight.js/lib/languages/css'),
    dart: () => import('highlight.js/lib/languages/dart'),
    xml: () => import('highlight.js/lib/languages/xml'),
    java: () => import('highlight.js/lib/languages/java'),
    javascript: () => import('highlight.js/lib/languages/javascript'),
    json: () => import('highlight.js/lib/languages/json'),
    lua: () => import('highlight.js/lib/languages/lua'),
    markdown: () => import('highlight.js/lib/languages/markdown'),
    python: () => import('highlight.js/lib/languages/python'),
    rust: () => import('highlight.js/lib/languages/rust'),
    shell: () => import('highlight.js/lib/languages/shell'),
    typescript: () => import('highlight.js/lib/languages/typescript'),
    plaintext: () => import('highlight.js/lib/languages/plaintext'),
    yaml: () => import('highlight.js/lib/languages/yaml'),
}

async function highlight(language: string, code: string): Promise<string | null> {
    try {
        if (!hljs.getLanguage(language)) {
            const load = highlightLanguages[language]
            if (!load) return null
            hljs.registerLanguage(language, (await load()).default)
        }
        return hljs.highlight(code, { language, ignoreIllegals: true }).value
    } catch {
        return null
    }
}

function wireRequest(request: NativeDisplayRequest, firstSlot: number | null) {
    const { character, quotes } = request
    return {
        text: wire(request.text),
        mode: request.mode,
        character: character && {
            assetWidth: character.assetWidth,
            chatId: character.chatId,
            chaId: wire(character.chaId),
            characterImage: wireOrNull(character.characterImage),
            userImage: wireOrNull(character.userImage),
            sourcesThrow: character.sourcesThrow,
        },
        hideAllImages: request.hideAllImages,
        firstSlot,
        imageReservation: request.imageReservation,
        thoughtMode: request.thoughtMode,
        cot: wire(request.labels.cot),
        toolCalled: wire(request.labels.toolCalled),
        error: wire(request.labels.error),
        quotes: {
            customQuotes: quotes.customQuotes,
            customQuotesData: quotes.customQuotesData?.map(wire) ?? null,
            unformatQuotes: quotes.unformatQuotes,
            blockquoteStyling: quotes.blockquoteStyling,
        },
        js: jsEngine(),
    }
}

/**
 * `parseMarkdownForDisplay` up to `trimMarkdown` in the app core: the render asks for one answer at a time and the
 * host gives it. A decline, a failure or a lost session hands the message back to the TS path; scripts that already
 * ran are not run again, since they have side effects.
 */
export async function renderDisplayNatively(
    request: NativeDisplayRequest,
    host: NativeDisplayHost,
    ready: Promise<void> | null = null,
): Promise<NativeDisplayResult> {
    const sources = new Map<string, InlayRenderSource>()
    const firstSlot = host.registry.nextSlot
    let scripted: { firstParsed: string, data: string } | null = null
    let session: number | null = null
    const declined = (reason: string): NativeDisplayResult => ({ kind: 'declined', reason, scripted })
    const call = async (command: string, args: Record<string, unknown>): Promise<Response | null> => {
        try {
            return await invoke<Response>(command, args)
        } catch {
            return null
        }
    }
    const answer = async (step: Exclude<Step, { kind: 'done' }>) => {
        switch (step.kind) {
            case 'assetLookups':
                return {
                    kind: step.kind,
                    found: step.lookups.map(({ tag, name }) => {
                        const found = host.lookup(tag, read(name))
                        return found ? { srcPaths: found.srcPaths.map(wire), ext: wireOrNull(found.ext) } : null
                    }),
                }
            case 'assetUrls':
                return { kind: step.kind, urls: (await Promise.all(step.requests.map(({ path, image }) => host.url(read(path), image)))).map(wire) }
            case 'scripts': {
                const firstParsed = read(step.text)
                const data = await host.scripts(firstParsed)
                scripted = { firstParsed, data }
                return { kind: step.kind, text: wire(data) }
            }
            case 'inlays': {
                const found = await host.inlays(step.ids.map(read))
                const inlays = []
                for (const [id, source] of found) {
                    if (!source) continue
                    sources.set(id, source)
                    inlays.push({ id: wire(id), type: source.type, mime: wire(source.mime), width: source.width ?? null, height: source.height ?? null })
                }
                return { kind: step.kind, inlays }
            }
            case 'math':
                return {
                    kind: step.kind,
                    values: step.tex.map((tex) => {
                        try {
                            return wire(katex.renderToString(read(tex), { displayMode: false, throwOnError: true, output: 'mathml' }))
                        } catch (error) {
                            console.error('KaTeX render error:', error)
                            return null
                        }
                    }),
                }
            case 'highlights': {
                const values = []
                for (const { language, code } of step.blocks) values.push(wireOrNull(await highlight(language, read(code))))
                return { kind: step.kind, values }
            }
        }
    }
    try {
        let [response] = await Promise.all([call('render_display_start', { request: wireRequest(request, firstSlot) }), ready])
        for (;;) {
            host.signal?.throwIfAborted()
            if (!response) return declined('invoke')
            if (!('step' in response)) return declined('declined' in response ? response.declined : response.failed)
            session = response.session
            const step = response.step
            if (step.kind === 'done') {
                // The slots are in the HTML, so they must be the ones the registry hands out now, each for an inlay
                // the render was given; the registry takes none of them otherwise.
                const registrations = step.registrations.map(({ slot, id }) => {
                    const key = read(id)
                    return { slot, id: key, source: sources.get(key) }
                })
                if (host.registry.nextSlot !== firstSlot
                    || registrations.some(({ slot, source }, index) => firstSlot === null || slot !== firstSlot + index || !source)) {
                    return declined('display-slots')
                }
                for (const { id, source } of registrations) host.registry.register(id, source!)
                return { kind: 'done', html: read(step.html), trim: step.trim, styleHistory: step.styleHistory }
            }
            const reply = await answer(step)
            host.signal?.throwIfAborted()
            response = await call('render_display_resume', { session, reply })
        }
    } finally {
        if (session !== null) void call('render_display_cancel', { session })
    }
}
