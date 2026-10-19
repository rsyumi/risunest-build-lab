import { beforeEach, describe, expect, it, vi } from 'vitest'

const f = vi.hoisted(() => ({ fetch: vi.fn(), request: vi.fn(), alert: vi.fn(), translatorType: 'deepl' }))
vi.mock('../storage/database.svelte', () => ({
    getDatabase: () => ({
        characters: [], presetRegex: [], translator: 'ko', translatorInputLanguage: 'en', aiModel: 'gpt',
        translatorType: f.translatorType, deeplOptions: { freeApi: true, key: 'synthetic' },
    }) as ReturnType<typeof import('../storage/database.svelte').getDatabase>,
    getCurrentCharacter: () => undefined,
    getCurrentChat: () => undefined,
} satisfies Partial<typeof import('../storage/database.svelte')>))
vi.mock('../globalApi.svelte', async importOriginal => ({ ...await importOriginal<typeof import('../globalApi.svelte')>(), globalFetch: f.fetch }))
vi.mock('../process/request/request', async importOriginal => ({ ...await importOriginal<typeof import('../process/request/request')>(), requestChatData: f.request }))
vi.mock('../alert', async importOriginal => ({ ...await importOriginal<typeof import('../alert')>(), alertError: f.alert }))
vi.mock('localforage', async () => (await import('../plugins/localforage.testSupport')).localforageModule(new Map()))

import { translate } from './translator'

beforeEach(() => {
    vi.clearAllMocks()
    f.translatorType = 'deepl'
    f.fetch.mockResolvedValue({ ok: false, data: 'synthetic unavailable' })
    f.request.mockResolvedValue({ type: 'fail', result: 'synthetic unavailable' })
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
