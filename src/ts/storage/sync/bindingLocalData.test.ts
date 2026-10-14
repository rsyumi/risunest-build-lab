import { beforeEach, expect, it, vi } from 'vitest'
const invoke = vi.hoisted(() => vi.fn())
vi.mock('@tauri-apps/api/core', () => ({ invoke }))
vi.mock('src/ts/process/modules', () => ({ moduleUpdate: vi.fn(), getModules: () => [] }))
vi.mock('src/ts/parser/parser.svelte', async () => (await import('../../parser/parser.testSupport')).parserStubModule())
import { hasLocalSharedBindingData } from './bindingLocalData'
import { normalizeDatabaseDefaults, type Database } from '../database.svelte'
function native() {
    return { library: structuredClone(normalizeDatabaseDefaults({} as Database)), characterCount: '0', managedAliasCount: '0', ordinaryPluginValueCount: '0', hypaValueCount: '0', pluginLocalValueCount: '0', opaqueSharedUnitCount: '0', sharedVariables: {}, pluginLocalParticipating: false }
}
beforeEach(() => { invoke.mockReset() })
it('a current fresh factory library has nothing to publish', async () => {
    invoke.mockResolvedValue(native())
    expect(await hasLocalSharedBindingData()).toBe(false)
})
it.each(['characterCount', 'managedAliasCount', 'ordinaryPluginValueCount', 'hypaValueCount', 'pluginLocalValueCount', 'opaqueSharedUnitCount'])('rejects a numeric native %s', async key => {
    invoke.mockResolvedValue({ ...native(), [key]: 0 })
    await expect(hasLocalSharedBindingData()).rejects.toThrow('Invalid binding content count')
})
it('accepts native count strings beyond Number precision', async () => {
    invoke.mockResolvedValue({ ...native(), ordinaryPluginValueCount: '9007199254740993' })
    expect(await hasLocalSharedBindingData()).toBe(true)
})
it.each([false, true])('plugin-local-only content needs a shared-state transfer only with participation %s', async participating => {
    invoke.mockResolvedValue({ ...native(), pluginLocalValueCount: '2', pluginLocalParticipating: participating })
    expect(await hasLocalSharedBindingData()).toBe(participating)
})
it.each([undefined, 1, 'true'])('rejects native plugin-local participation %s', async value => {
    invoke.mockResolvedValue({ ...native(), pluginLocalParticipating: value })
    await expect(hasLocalSharedBindingData()).rejects.toThrow('Invalid binding content')
})
it('a factory library has no characters, so one reported character is shared data', async () => {
    expect(native().library.characters).toEqual([])
    invoke.mockResolvedValue({ ...native(), characterCount: '1' })
    expect(await hasLocalSharedBindingData()).toBe(true)
})

it('generated IDs assigned to factory presets and personas are not user edits', async () => {
    const content = native()
    Object.assign(content.library.botPresets[0], { id: 'generated-preset' })
    Object.assign(content.library.personas[0], { id: 'generated-persona' })
    invoke.mockResolvedValue(content)
    expect(await hasLocalSharedBindingData()).toBe(false)
})
