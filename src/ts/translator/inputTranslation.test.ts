import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'

const f = vi.hoisted(() => ({ fetch: vi.fn(), request: vi.fn(), alert: vi.fn(), translatorType: 'deepl', translator: 'ko', deeplXUrl: 'https://deeplx-a.invalid', experimentalGoogle: false }))
vi.mock('../storage/database.svelte', () => ({
    getDatabase: () => ({
        characters: [], presetRegex: [], translator: f.translator, translatorInputLanguage: 'en', aiModel: 'gpt',
        translatorType: f.translatorType, deeplOptions: { freeApi: true, key: 'synthetic' },
        deeplXOptions: { url: f.deeplXUrl, token: '' }, noWaitForTranslate: true,
        useExperimentalGoogleTranslator: f.experimentalGoogle,
    }) as ReturnType<typeof import('../storage/database.svelte').getDatabase>,
    getCurrentCharacter: () => undefined,
    getCurrentChat: () => undefined,
} satisfies Partial<typeof import('../storage/database.svelte')>))
vi.mock('../globalApi.svelte', async importOriginal => ({ ...await importOriginal<typeof import('../globalApi.svelte')>(), globalFetch: f.fetch }))
vi.mock('../process/request/request', async importOriginal => ({ ...await importOriginal<typeof import('../process/request/request')>(), requestChatData: f.request }))
vi.mock('../alert', async importOriginal => ({ ...await importOriginal<typeof import('../alert')>(), alertError: f.alert }))
vi.mock('localforage', async () => (await import('../plugins/localforage.testSupport')).localforageModule(new Map()))

import { getDatabase } from '../storage/database.svelte'
import { setRuntimePerformanceProfile } from '../runtimePerformanceProfile'
import { doingChat } from '../process/generationState'
import { translate, translateHTML, type TranslateHTMLContext } from './translator'

function liveRowContext(): TranslateHTMLContext {
    return {
        scriptContext: {
            presetRegex: [], moduleRegexScripts: [], moduleAssets: [],
            dynamicAssets: false, dynamicAssetsEditDisplay: false,
            parserContext: { database: getDatabase(), character: { type: 'simple', customscript: [], chaId: 'live' } },
        },
        liveProjection: true,
    } as unknown as TranslateHTMLContext
}

beforeEach(() => {
    vi.clearAllMocks()
    f.translatorType = 'deepl'
    f.translator = 'ko'
    f.deeplXUrl = 'https://deeplx-a.invalid'
    f.experimentalGoogle = false
    f.fetch.mockResolvedValue({ ok: false, data: 'synthetic unavailable' })
    f.request.mockResolvedValue({ type: 'fail', result: 'synthetic unavailable' })
})

afterEach(() => {
    doingChat.set(false)
    vi.unstubAllGlobals()
})

describe('translation error ownership', () => {
    it.each(['deepl', 'llm'])('leaves repeated %s failures to the input owner and never caches a failed input as translated', async provider => {
        f.translatorType = provider
        const draft = `${provider} input draft`
        for (let attempt = 0; attempt < 2; attempt++) {
            await expect(translate(draft, false, undefined, 'throw')).rejects.toThrow('synthetic unavailable')
        }
        expect(f.alert).not.toHaveBeenCalled()
        f.fetch.mockResolvedValue({ ok: true, data: { translations: [{ text: 'Translated draft' }] } })
        f.request.mockResolvedValue({ type: 'success', result: 'Translated draft' })
        await expect(translate(draft, false, undefined, 'throw')).resolves.toBe('Translated draft')
    })
    it.each(['deepl', 'llm'])('keeps the existing %s alert and original text for callers using the default mode', async provider => {
        f.translatorType = provider
        const draft = `${provider} explicit translation`
        await expect(translate(draft, false)).resolves.toBe(draft)
        expect(f.alert).toHaveBeenCalledExactlyOnceWith(provider === 'deepl' ? 'ERR::DeepL API Errorsynthetic unavailable' : 'synthetic unavailable')
    })
    it.each(['streaming', 'multiline'])('leaves unexpected %s responses to the input owner', async type => {
        f.translatorType = 'llm'
        f.request.mockResolvedValue({ type, result: [] })
        await expect(translate(`${type} input`, false, undefined, 'throw')).rejects.toThrow('Unexpected response type')
        expect(f.alert).not.toHaveBeenCalled()
    })
})

describe('live chat rows', () => {
    it('reuses a translation made for the same live row text', async () => {
        f.fetch.mockResolvedValue({ ok: true, data: { translations: [{ text: 'Live translated' }] } })
        const text = 'live row text that renders again'
        await expect(translate(text, false, liveRowContext())).resolves.toBe('Live translated')
        await expect(translate(text, false, liveRowContext())).resolves.toBe('Live translated')
        expect(f.fetch).toHaveBeenCalledOnce()
    })
    it('leaves a live row untranslated while a reply is generated', async () => {
        f.fetch.mockResolvedValue({ ok: true, data: { translations: [{ text: 'Streaming translated' }] } })
        doingChat.set(true)
        await expect(translateHTML('<p>streaming reply</p>', false, '', 0, false, liveRowContext())).resolves.toBe('<p>streaming reply</p>')
        expect(f.fetch).not.toHaveBeenCalled()
    })
    it('keeps the translations made earlier for a live row while a reply is generated', async () => {
        f.fetch.mockImplementation(async (_url: string, init: { body: { text: string[] } }) => (
            { ok: true, data: { translations: [{ text: `KO ${init.body.text[0]}` }] } }
        ))
        await expect(translateHTML('<p>earlier reply</p>', false, '', 0, false, liveRowContext())).resolves.toBe('<p>KO earlier reply</p>')
        doingChat.set(true)
        await expect(translateHTML('<p>earlier reply</p>', false, '', 0, false, liveRowContext())).resolves.toBe('<p>KO earlier reply</p>')
        await expect(translateHTML('<p>earlier reply</p><p>newer reply</p>', false, '', 0, false, liveRowContext()))
            .resolves.toBe('<p>earlier reply</p><p>newer reply</p>')
        expect(f.fetch).toHaveBeenCalledOnce()
    })
    it('translates the same text again for another target language', async () => {
        f.fetch.mockImplementation(async (_url: string, init: { body: { target_lang: string } }) => (
            { ok: true, data: { translations: [{ text: `in ${init.body.target_lang}` }] } }
        ))
        const text = 'text read in two languages'
        await expect(translate(text, false)).resolves.toBe('in KO')
        await expect(translate('in KO', true)).resolves.toBe(text)
        f.translator = 'ja'
        await expect(translate(text, false)).resolves.toBe('in JA')
        expect(f.fetch).toHaveBeenCalledTimes(2)
    })
    it('translates a live row again after the DeepLX server changes', async () => {
        f.translatorType = 'deeplX'
        f.fetch.mockImplementation(async (url: string) => (
            { ok: true, data: { data: url.startsWith('https://deeplx-a.invalid') ? 'from server A' : 'from server B' } }
        ))
        await expect(translateHTML('<p>live row text</p>', false, '', 0, false, liveRowContext())).resolves.toBe('<p>from server A</p>')
        f.deeplXUrl = 'https://deeplx-b.invalid'
        await expect(translateHTML('<p>live row text</p>', false, '', 0, false, liveRowContext())).resolves.toBe('<p>from server B</p>')
    })
    it('translates a live row again after experimental Google translation is enabled', async () => {
        f.translatorType = 'google'
        vi.stubGlobal('userScriptFetch', true)
        vi.stubGlobal('fetch', vi.fn(async () => new Response(JSON.stringify([[['standard Google']]]))))
        f.fetch.mockResolvedValue({ ok: true, data: '<div class="result-container">experimental Google</div>' })
        const html = '<p>Google setting cache fixture</p>'
        await expect(translateHTML(html, false, '', 0, false, liveRowContext())).resolves.toBe('<p>standard Google</p>')
        f.experimentalGoogle = true
        await expect(translateHTML(html, false, '', 0, false, liveRowContext())).resolves.toBe('<p>experimental Google</p>')
        f.experimentalGoogle = false
        await expect(translateHTML(html, false, '', 0, false, liveRowContext())).resolves.toBe('<p>standard Google</p>')
    })
    it('keeps translations within the memory budget of the device profile', async () => {
        f.fetch.mockResolvedValue({ ok: true, data: { translations: [{ text: 'Long translated' }] } })
        setRuntimePerformanceProfile('low-spec')
        try {
            const text = 'x'.repeat(3 * 1024 * 1024)
            await translate(text, false)
            await translate(text, false)
        } finally {
            setRuntimePerformanceProfile('normal')
        }
        expect(f.fetch).toHaveBeenCalledTimes(2)
    })
})
