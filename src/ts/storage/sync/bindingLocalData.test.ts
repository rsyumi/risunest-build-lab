import { beforeEach, expect, it, vi } from 'vitest'
const invoke = vi.hoisted(() => vi.fn())
vi.mock('@tauri-apps/api/core', () => ({ invoke }))
vi.mock('src/ts/process/modules', () => ({ moduleUpdate: vi.fn(), getModules: () => [] }))
vi.mock('src/ts/parser/parser.svelte', () => ({ risuChatParser: (text: string) => text }))
import { hasLocalBindingData, hasLocalSharedBindingData, inspectLocalBindingData } from './bindingLocalData'
import { normalizeDatabaseDefaults, type Database } from '../database.svelte'
import { DBState } from 'src/ts/stores.svelte'
function native() {
    return { library: structuredClone(normalizeDatabaseDefaults({} as Database)), managedAliasCount: '0', ordinaryPluginValueCount: '0', hypaValueCount: '0', pluginLocalValueCount: '0', opaqueSharedUnitCount: '0', sharedVariables: {} }
}
DBState.db = native().library
beforeEach(() => { invoke.mockReset(); DBState.db = native().library })
it('a current fresh factory library needs no replacement acknowledgement', async () => {
    invoke.mockResolvedValue(native())
    expect(await hasLocalBindingData()).toBe(false)
})
it.each(['managedAliasCount', 'ordinaryPluginValueCount', 'hypaValueCount', 'pluginLocalValueCount', 'opaqueSharedUnitCount'])('rejects a numeric native %s', async key => {
    invoke.mockResolvedValue({ ...native(), [key]: 0 })
    await expect(inspectLocalBindingData()).rejects.toThrow('Invalid binding content count')
})
it('keeps native count strings exact without Number conversion', async () => {
    invoke.mockResolvedValue({ ...native(), pluginLocalValueCount: '9007199254740993' })
    expect((await inspectLocalBindingData()).pluginLocalValueCount).toBe('9007199254740993')
    expect(await hasLocalBindingData()).toBe(true)
})
it('plugin-local-only content needs acknowledgement but no shared-state transfer', async () => {
    invoke.mockResolvedValue({ ...native(), pluginLocalValueCount: '2' })
    expect(await hasLocalBindingData()).toBe(true)
    expect(await hasLocalSharedBindingData()).toBe(false)
})
it('counts Hypa memo content stored in the materialized library', async () => {
    const content = native()
    content.library.characters = [{ chaId: 'synthetic', name: 'Test', chats: [{ id: 'chat', hypaV3Data: { memo: 'synthetic memo' }, message: [] }] }] as unknown as Database['characters']
    invoke.mockResolvedValue(content)
    expect(await hasLocalBindingData()).toBe(true)
})

it('generated IDs assigned to factory presets and personas are not user edits', async () => {
    const content = native()
    Object.assign(content.library.botPresets[0], { id: 'generated-preset' })
    Object.assign(content.library.personas[0], { id: 'generated-persona' })
    invoke.mockResolvedValue(content)
    expect(await hasLocalBindingData()).toBe(false)
})
