import { beforeEach, describe, expect, it, vi } from 'vitest'
const invoke = vi.hoisted(() => vi.fn())
vi.mock('@tauri-apps/api/core', () => ({ invoke }))
vi.mock('src/ts/process/modules', () => ({ moduleUpdate: vi.fn(), getModules: () => [] }))
vi.mock('src/ts/parser/parser.svelte', () => ({ risuChatParser: (text: string) => text }))
import { hasLocalBindingData, hasLocalLibraryContent, hasLocalSharedBindingData, inspectLocalBindingData } from './bindingLocalData'
import { normalizeDatabaseDefaults, type Database } from '../database.svelte'
import { prepareDatabaseForBootstrap } from '../databasePreparation'
import { NEW_DATABASE_SEED } from '../persistentBootstrap'
import { DBState } from 'src/ts/stores.svelte'
function native() {
    return { library: structuredClone(normalizeDatabaseDefaults({} as Database)), characterCount: '0', managedAliasCount: '0', ordinaryPluginValueCount: '0', hypaValueCount: '0', pluginLocalValueCount: '0', opaqueSharedUnitCount: '0', sharedVariables: {}, pluginLocalParticipating: false }
}
DBState.db = native().library
beforeEach(() => { invoke.mockReset(); DBState.db = native().library })
it('a current fresh factory library needs no replacement acknowledgement', async () => {
    invoke.mockResolvedValue(native())
    expect(await hasLocalBindingData()).toBe(false)
})
it.each(['characterCount', 'managedAliasCount', 'ordinaryPluginValueCount', 'hypaValueCount', 'pluginLocalValueCount', 'opaqueSharedUnitCount'])('rejects a numeric native %s', async key => {
    invoke.mockResolvedValue({ ...native(), [key]: 0 })
    await expect(inspectLocalBindingData()).rejects.toThrow('Invalid binding content count')
})
it('keeps native count strings exact without Number conversion', async () => {
    invoke.mockResolvedValue({ ...native(), pluginLocalValueCount: '9007199254740993' })
    expect((await inspectLocalBindingData()).pluginLocalValueCount).toBe('9007199254740993')
    expect(await hasLocalBindingData()).toBe(true)
})
it.each([false, true])('plugin-local-only content needs acknowledgement and a shared-state transfer only with participation %s', async participating => {
    invoke.mockResolvedValue({ ...native(), pluginLocalValueCount: '2', pluginLocalParticipating: participating })
    expect(await hasLocalBindingData()).toBe(true)
    expect(await hasLocalSharedBindingData()).toBe(participating)
})
it.each([undefined, 1, 'true'])('rejects native plugin-local participation %s', async value => {
    invoke.mockResolvedValue({ ...native(), pluginLocalParticipating: value })
    await expect(hasLocalSharedBindingData()).rejects.toThrow('Invalid binding content')
})
it('a factory library has no characters, so one reported character needs acknowledgement', async () => {
    expect(native().library.characters).toEqual([])
    invoke.mockResolvedValue({ ...native(), characterCount: '1' })
    expect(await hasLocalBindingData()).toBe(true)
    expect(await hasLocalSharedBindingData()).toBe(true)
})

it('generated IDs assigned to factory presets and personas are not user edits', async () => {
    const content = native()
    Object.assign(content.library.botPresets[0], { id: 'generated-preset' })
    Object.assign(content.library.personas[0], { id: 'generated-persona' })
    invoke.mockResolvedValue(content)
    expect(await hasLocalBindingData()).toBe(false)
})

describe('library content in the onboarding', () => {
    // What a new device stores, as the native store reports it, with the language the onboarding chose.
    async function newDevice(language = 'ko') {
        const { database } = await prepareDatabaseForBootstrap({ ...NEW_DATABASE_SEED } as Database)
        const library = structuredClone({ ...database, language, characters: [] }) as Record<string, unknown>
        delete library.account
        return { ...native(), library }
    }
    it.each(['en', 'ko'])('finds none on a new device that the factory comparison counts as data, language %s', async language => {
        invoke.mockResolvedValue(await newDevice(language))
        expect(await hasLocalBindingData()).toBe(true)
        expect(await hasLocalLibraryContent()).toBe(false)
    })
    it.each([
        ['a character', (content: Awaited<ReturnType<typeof newDevice>>) => { content.characterCount = '1' }],
        ['a plugin value', (content: Awaited<ReturnType<typeof newDevice>>) => { content.ordinaryPluginValueCount = '1' }],
        ['an edited preset', (content: Awaited<ReturnType<typeof newDevice>>) => { (content.library.botPresets as { name: string }[])[0].name = 'User edit' }],
        ['a global lorebook', (content: Awaited<ReturnType<typeof newDevice>>) => { content.library.loreBook = [{ name: 'Lore', data: [] }] }],
    ])('finds %s', async (_name, edit) => {
        const content = await newDevice()
        edit(content)
        invoke.mockResolvedValue(content)
        expect(await hasLocalLibraryContent()).toBe(true)
    })
})
