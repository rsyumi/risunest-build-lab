import { afterEach, expect, it, vi } from 'vitest'
import { mount, unmount } from 'svelte'
import { writable } from 'svelte/store'
import { language } from 'src/lang'
import { DBState } from 'src/ts/stores.svelte'
import { tokenizePreset } from 'src/ts/process/prompt'
import { getCharToken } from 'src/ts/tokenizer'
import OtherBotSettings from './OtherBotSettings.svelte'

vi.mock('src/ts/stores.svelte', () => {
    const DBState = $state({ db: {} })
    return { DBState, selectedCharID: writable(0) }
})
vi.mock('src/ts/process/prompt', () => ({ tokenizePreset: vi.fn() }))
vi.mock('src/ts/tokenizer', async () => (await import('src/ts/tokenizer.testSupport')).tokenizerStubModule({
    getCharToken: vi.fn(),
}))
vi.mock('src/ts/process/memory/hypav3', () => ({ createHypaV3Preset: vi.fn() }))
vi.mock('src/ts/globalApi.svelte', () => ({ saveAsset: vi.fn(), downloadFile: vi.fn(), globalFetch: vi.fn() }))
vi.mock('src/ts/util', () => ({ selectSingleFile: vi.fn() }))
vi.mock('src/ts/characters', () => ({ getCharImage: vi.fn(async () => '') }))
vi.mock('src/ts/alert', () => ({ alertError: vi.fn(), alertInput: vi.fn(), alertConfirm: vi.fn(), alertNormal: vi.fn() }))
vi.mock('src/lib/Others/Help.svelte', () => ({ default: () => {} }))
vi.mock('src/lib/UI/GUI/TextAreaInput.svelte', () => ({ default: () => {} }))

let instance: ReturnType<typeof mount> | undefined

function deferred<T>() {
    let resolve!: (value: T) => void
    const promise = new Promise<T>((done) => { resolve = done })
    return { promise, resolve }
}

afterEach(async () => {
    if (instance) await unmount(instance)
    instance = undefined
    document.body.replaceChildren()
})

it('counts the prompt template and the character together for the HypaV3 memory ratio', async () => {
    const template = deferred<number>()
    const character = deferred<{ persistant: number, dynamic: number }>()
    vi.mocked(tokenizePreset).mockReturnValue(template.promise)
    vi.mocked(getCharToken).mockReturnValue(character.promise)
    DBState.db = {
        useLegacyGUI: false,
        hypaV3: true,
        hypaV3PresetId: 0,
        hypaV3Presets: [{ name: 'Default', settings: { recentMemoryRatio: 0.4, similarMemoryRatio: 0.4 } }],
        characters: [{ name: 'Char', chaId: 'char' }],
        promptTemplate: [],
        maxContext: 1000,
        maxResponse: 100,
        loreBookToken: 200,
        supaModelType: 'none',
    } as any
    instance = mount(OtherBotSettings, { target: document.body })

    await vi.waitFor(() => expect(getCharToken).toHaveBeenCalledOnce())
    expect(tokenizePreset).toHaveBeenCalledOnce()
    template.resolve(100)
    character.resolve({ persistant: 50, dynamic: 500 })

    // One minus (100 + 50 + 200 + 100 * 3) / 1000.
    const ratio = () => [...document.querySelectorAll('span')]
        .find((span) => span.textContent === language.hypaV3Settings.maxMemoryTokensRatioLabel)
        ?.nextElementSibling as HTMLInputElement | undefined
    await vi.waitFor(() => expect(ratio()?.value).toBe('0.35'))
})
