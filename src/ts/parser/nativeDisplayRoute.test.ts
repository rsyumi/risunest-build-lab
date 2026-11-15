// @vitest-environment jsdom

import { beforeEach, expect, it, vi } from 'vitest'
import { flushSync } from 'svelte'
import { toStore, writable } from 'svelte/store'

const selection = vi.hoisted(() => {
    const selected = { chaId: 'route-character', image: '', chatPage: 0, chats: [{ id: 'route-chat' }] }
    return { selected, current: selected as typeof selected | undefined }
})

vi.mock('../platform', async () => (await import('../platform.testSupport')).platformModule({ runtime: 'tauri', os: 'windows' }))
vi.mock('@tauri-apps/api/core', async () => (await import('../globalApi.testSupport')).tauriCoreModule())
vi.mock('@tauri-apps/api/webviewWindow', async () => (await import('../globalApi.testSupport')).webviewWindowModule())

vi.mock(import('../storage/database.svelte'), () => ({
    appVer: '1.0.0',
    getCurrentCharacter: () => selection.current,
    getDatabase: () => ({}),
} as unknown as typeof import('../storage/database.svelte')))

vi.mock(import('../globalApi.svelte'), () => ({
    aiWatermarkingLawApplies: () => false,
    getFileSrc: async (path: string) => `resolved:${path}`,
    getFileImageSource: async () => null,
}))

vi.mock(import('../stores.svelte'), async () => ({
    DBState: {
        db: (await import('./nativeDisplayRoute.testSupport.svelte')).reactiveCopy({
            assetWidth: -1,
            hideAllImages: false,
            legacyMediaFindings: false,
            assetMaxDifference: 1,
            customQuotes: false,
            customQuotesData: null,
            unformatQuotes: false,
            blockquoteStyling: false,
            characters: [],
            globalChatVariables: {},
            templateDefaultVariables: '',
        }),
    },
    selIdState: { selId: -1 },
    selectedCharID: writable(-1),
} as unknown as typeof import('../stores.svelte')))

vi.mock(import('../process/modules'), () => ({
    getModuleAssets: () => [],
    getModuleLorebooks: () => [],
    getModules: () => [],
}))

vi.mock(import('../process/scripts'), () => ({
    processScriptFull: vi.fn(async (_char: unknown, data: string) => ({ data: data.replace('first', '**scripted**'), emoChanged: false })),
} as unknown as typeof import('../process/scripts')))

import { invoke } from '@tauri-apps/api/core'
import { DBState } from '../stores.svelte'
import { ParseMarkdown, type simpleCharacterArgument } from './parser.svelte'
import { processScriptFull } from '../process/scripts'
import { createLocalDeviceMarkers } from '../storage/deviceMarkers'
import { loadDeviceSettings, updateNativeDisplayRender } from '../storage/deviceSettings'

const character: simpleCharacterArgument = { type: 'simple', chaId: 'route-character', customscript: [], additionalAssets: [], emotionImages: [] }

/** Answers start and resume with `responses` in turn. */
function serve(responses: unknown[]) {
    vi.mocked(invoke).mockImplementation(async (command) => command === 'render_display_cancel' ? null : responses.shift())
}

const commands = () => vi.mocked(invoke).mock.calls.map(([command]) => command)

/** The character part of the last request that started a render. */
const startedCharacter = () => (vi.mocked(invoke).mock.calls.filter(([command]) => command === 'render_display_start').at(-1)?.[1] as
    { request: { character: { sourcesThrow: boolean } } }).request.character

beforeEach(() => {
    localStorage.clear()
    loadDeviceSettings(createLocalDeviceMarkers(localStorage))
    vi.mocked(invoke).mockReset()
    vi.mocked(processScriptFull).mockClear()
    selection.current = selection.selected
})

it('renders a message the routing rule leaves to TS in TS', async () => {
    serve([])
    expect(await ParseMarkdown('first', character, 'normal', 0)).toContain('<strong>scripted</strong>')
    expect(commands()).not.toContain('render_display_start')
})

it('sanitizes the HTML the app core renders, and returns it as it is for notrim', async () => {
    expect(updateNativeDisplayRender(true)).toBe(true)
    const done = (trim: boolean) => ({ session: null, step: {
        kind: 'done', html: '<p>native</p><img src="x" onerror="alert(1)">', registrations: [], trim, styleHistory: false } })
    serve([{ session: 4, step: { kind: 'scripts', text: 'first' } }, done(true)])
    const html = await ParseMarkdown('first', character, 'normal', 0)
    expect(html).toContain('<p>native</p>')
    expect(html).not.toContain('onerror')
    expect(vi.mocked(processScriptFull).mock.calls.map(([, data]) => data)).toEqual(['first'])

    serve([done(false)])
    expect(await ParseMarkdown('first', character, 'notrim', 0)).toBe('<p>native</p><img src="x" onerror="alert(1)">')
})

it('goes on in TS from the script result after a decline, without running the scripts again', async () => {
    serve([])
    const expected = await ParseMarkdown('first', character, 'normal', 0)
    vi.mocked(processScriptFull).mockClear()
    expect(updateNativeDisplayRender(true)).toBe(true)
    serve([{ session: 4, step: { kind: 'scripts', text: 'first' } }, { declined: 'markdown-x' }])
    expect(await ParseMarkdown('first', character, 'normal', 0)).toBe(expected)
    expect(vi.mocked(processScriptFull)).toHaveBeenCalledTimes(1)
    expect(commands()).toEqual(['render_display_start', 'render_display_resume', 'render_display_cancel'])

    vi.mocked(processScriptFull).mockClear()
    serve([{ declined: 'markdown-x' }])
    expect(await ParseMarkdown('first', character, 'normal', 0)).toBe(expected)
    expect(vi.mocked(processScriptFull)).toHaveBeenCalledTimes(1)
})

it('tells the app core where TS throws on a source tag, so its decline leaves TS to throw', async () => {
    expect(updateNativeDisplayRender(true)).toBe(true)
    selection.current = undefined
    serve([{ declined: 'asset-source' }])
    await expect(ParseMarkdown('{{source::char}}', character, 'normal', 0)).rejects.toThrow("reading 'image'")
    expect(startedCharacter().sourcesThrow).toBe(true)

    serve([{ declined: 'asset-source' }])
    await ParseMarkdown('{{source::char}}', character, 'normal', 0, {}, { characterImageSource: '' })
    expect(startedCharacter().sourcesThrow).toBe(false)
    selection.current = selection.selected
    serve([{ declined: 'asset-source' }])
    await ParseMarkdown('{{source::char}}', character, 'normal', 0)
    expect(startedCharacter().sourcesThrow).toBe(false)
})

const settingKeys = ['assetWidth', 'hideAllImages', 'legacyMediaFindings', 'assetMaxDifference',
    'customQuotes', 'customQuotesData', 'unformatQuotes', 'blockquoteStyling'] as const

/** The settings whose change runs a reactive caller of the render of `text` again. */
async function trackedSettings(text: string, char: simpleCharacterArgument | null, base: Record<string, unknown> = {}) {
    const db = DBState.db as unknown as Record<string, unknown>
    const saved = Object.fromEntries(settingKeys.map((key) => [key, db[key]]))
    Object.assign(db, base)
    const renders: Promise<unknown>[] = []
    let runs = 0
    const stop = toStore(() => {
        runs++
        renders.push(ParseMarkdown(text, char, 'normal', 0))
    }).subscribe(() => {})
    const tracked: string[] = []
    for (const key of settingKeys) {
        const before = runs
        const value = db[key]
        db[key] = typeof value === 'boolean' ? !value : typeof value === 'number' ? value + 1 : ['<', '>', '[', ']']
        flushSync()
        if (runs > before) tracked.push(key)
        db[key] = value
        flushSync()
    }
    stop()
    await Promise.all(renders)
    Object.assign(db, saved)
    return tracked
}

it('makes a reactive caller track the settings the TS stages track', async () => {
    vi.mocked(invoke).mockImplementation(async (command) => command === 'render_display_cancel' ? null : { declined: 'markdown-x' })
    const renders: [string, simpleCharacterArgument | null, Record<string, unknown>?][] = [
        ['plain', character], ['plain', null], ['{{img::a}}', character], ['{{emotion::a}}', character],
        ['{{source::char}}', character], ['{{source::other}}', character], ['{{img::a}}', character, { hideAllImages: true }],
    ]
    for (const [text, char, base] of renders) {
        expect(updateNativeDisplayRender(false)).toBe(true)
        const ts = await trackedSettings(text, char, base)
        expect(updateNativeDisplayRender(true)).toBe(true)
        expect(await trackedSettings(text, char, base), `${text} ${char ? 'with' : 'without'} a character`).toEqual(ts)
    }
    expect(updateNativeDisplayRender(false)).toBe(true)
    expect(await trackedSettings('{{img::a}}', character)).toEqual(['assetWidth', 'hideAllImages', 'legacyMediaFindings', 'assetMaxDifference'])
    expect(await trackedSettings('plain', null)).toEqual([])
})
