import { describe, expect, it, vi } from 'vitest'
const native = vi.hoisted(() => ({ invoke: vi.fn() }))
vi.mock('@tauri-apps/api/core', async original => ({ ...await original<typeof import('@tauri-apps/api/core')>(), invoke: native.invoke } satisfies typeof import('@tauri-apps/api/core')))
vi.mock('../platform', async () => (await import('../platform.testSupport')).platformModule({ runtime: 'tauri', os: 'ios' }))
import { importNativePresetFromPicker, importPreparedNativePreset } from './nativePresetImportRoute'
import { prepareNativePresetImport } from './nativeFileJobs'
import type { NativeFileJobDependencies, PreparedNativePresetReceipt } from './nativeFileJobs'

function host(mapping: unknown, bytes = Uint8Array.of(0, 255, 128)) {
    let released = false
    const commands: string[] = []
    const dependencies: NativeFileJobDependencies = {
        isTauri: () => true, wait: async () => {},
        invoke: async (command, args) => {
            commands.push(command)
            if (command === 'native_file_job_start') return { jobId: 'preset-job' }
            if (command === 'native_file_job_status') return { jobId: 'preset-job', kind: 'prepare-preset-import', state: 'succeeded', phase: 'complete', progress: { completedBytes: bytes.length, completedItems: 0 } }
            if (command === 'native_file_job_prepared_preset') return { displayName: 'synthetic.risup', sourceBytes: bytes.length, nativeMapping: mapping }
            if (command === 'native_file_job_preset_source') {
                expect(released).toBe(false)
                return Array.from(bytes.subarray(args!.offset as number, (args!.offset as number) + (args!.length as number)))
            }
            if (command === 'native_file_job_forget') { released = true; return true }
            if (command === 'native_file_job_cancel') return 'requested'
            throw new Error(command)
        },
    }
    return { dependencies, commands, released: () => released }
}

describe('native preset import route', () => {
    it('uses one mapped identity and releases only after append completes', async () => {
        const h = host({ outcome: 'mapped', preset: { id: 'native-id', name: 'Synthetic', opaque: [1] } })
        const receipt = await prepareNativePresetImport({ type: 'desktopPath', path: 'synthetic.risup' }, 'synthetic.risup', {}, h.dependencies)
        const imported: unknown[] = []
        await importPreparedNativePreset(receipt, {
            append: async preset => { expect(h.released()).toBe(false); imported.push(preset) },
            importLegacy: async () => { throw new Error('Mapped presets do not decode again') },
        })
        expect(imported).toEqual([{ id: 'native-id', name: 'Synthetic', opaque: [1] }])
        expect(h.released()).toBe(true)
        expect(h.commands).not.toContain('native_file_job_preset_source')
    })

    it.each([
        { outcome: 'declined', reason: 'unsupported', name: 'msgpackValue' },
        { outcome: 'mapped', preset: { id: '' } },
        { outcome: 'mapped', preset: [] },
    ])('passes original owned bytes to the compatibility importer for %j', async mapping => {
        const h = host(mapping)
        const receipt = await prepareNativePresetImport({ type: 'androidSpool', token: 'synthetic-token' }, 'synthetic.risup', {}, h.dependencies)
        const failure = new Error('exact legacy decoder failure')
        await expect(importPreparedNativePreset(receipt, {
            append: async () => { throw new Error('Must decline') },
            importLegacy: async file => {
                expect(h.released()).toBe(false)
                expect(file).toEqual({ name: 'synthetic.risup', data: Uint8Array.of(0, 255, 128) })
                throw failure
            },
        })).rejects.toBe(failure)
        expect(h.released()).toBe(true)
    })

    it('does not retry legacy import after an uncertain append failure', async () => {
        const h = host({ outcome: 'mapped', preset: { id: 'final-id' } })
        const receipt = await prepareNativePresetImport({ type: 'iosScoped', token: 'synthetic-token' }, 'synthetic.json', {}, h.dependencies)
        const failure = new Error('committed refresh failed')
        const legacy = vi.fn()
        await expect(importPreparedNativePreset(receipt, { append: async () => { throw failure }, importLegacy: legacy })).rejects.toBe(failure)
        expect(legacy).not.toHaveBeenCalled()
        expect(h.released()).toBe(true)
    })

    it('cancels before append and treats picker cancellation as handled', async () => {
        const abort = new AbortController()
        abort.abort()
        const append = vi.fn()
        const release = vi.fn(async () => {})
        await expect(importPreparedNativePreset({ displayName: 'synthetic.json', sourceBytes: 0, readSource: async () => new Uint8Array(), nativeMapping: { outcome: 'mapped', preset: { id: 'a' } }, release } satisfies PreparedNativePresetReceipt,
            { append, importLegacy: vi.fn() }, { signal: abort.signal })).rejects.toMatchObject({ name: 'AbortError' })
        expect(append).not.toHaveBeenCalled()
        expect(release).toHaveBeenCalledOnce()
        await expect(importNativePresetFromPicker({ append, importLegacy: vi.fn() }, {}, {
            isNative: () => true, chooseSource: async () => null, prepare: async () => { throw new Error('No preparation') },
        })).resolves.toBe(true)
        await expect(importNativePresetFromPicker({ append, importLegacy: vi.fn() }, {}, {
            isNative: () => false, chooseSource: async () => { throw new Error('Web keeps TS') }, prepare: prepareNativePresetImport,
        })).resolves.toBe(false)
    })
})

it('discards a picker source cancelled before the native job can claim it', async () => {
    const abort = new AbortController()
    const discarded: unknown[] = []
    const prepared: unknown[] = []
    await expect(importNativePresetFromPicker({ append: async () => {}, importLegacy: async () => {} }, { signal: abort.signal }, {
        isNative: () => true,
        chooseSource: async () => { abort.abort(); return { source: { type: 'androidSpool', token: 'synthetic-spool' }, displayName: 'synthetic.risup' } },
        discardSource: async source => { discarded.push(source) },
        prepare: async (...args) => { prepared.push(args); throw new Error('Must not claim a cancelled source') },
    })).rejects.toMatchObject({ name: 'AbortError' })
    expect(discarded).toEqual([{ type: 'androidSpool', token: 'synthetic-spool' }])
    expect(prepared).toEqual([])
})

it.each(['succeeded', 'failed'] as const)('uses the real iOS staged-path picker and cleans its %s preparation', async state => {
    const events: string[] = []
    native.invoke.mockImplementation(async (command: string, args?: Record<string, unknown>) => {
        events.push(command)
        if (command === 'app_paths_roots') return { data: '/synthetic/data' }
        if (command === 'plugin:ios-native|pick_file') return { cancelled: false, path: '/synthetic/ios-stage/preset.risup', name: 'actual.risup', bytes: 10 }
        if (command === 'native_file_job_start') {
            expect(args).toEqual({ request: { kind: 'prepare-preset-import', source: { type: 'desktopPath', path: '/synthetic/ios-stage/preset.risup' }, displayName: 'actual.risup' } })
            return { jobId: 'ios-preset' }
        }
        if (command === 'native_file_job_status') return { jobId: 'ios-preset', kind: 'prepare-preset-import', state, phase: 'complete', progress: { completedBytes: 10, completedItems: 0 }, ...(state === 'failed' ? { error: { code: 'synthetic-failure', message: 'Synthetic preparation failed' } } : {}) }
        if (command === 'native_file_job_prepared_preset') return { displayName: 'actual.risup', sourceBytes: 10, nativeMapping: { outcome: 'mapped', preset: { id: 'ios-final-id', name: 'Synthetic' } } }
        if (command === 'plugin:ios-native|discard_file') { expect(args).toEqual({ path: '/synthetic/ios-stage/preset.risup' }); return }
        if (command === 'native_file_job_forget' || command === 'native_file_job_cancel') return true
        throw new Error(command)
    })
    const appended: unknown[] = []
    const operation = importNativePresetFromPicker({ append: async preset => { appended.push(preset) }, importLegacy: async () => { throw new Error('Mapped preset') } })
    if (state === 'failed') await expect(operation).rejects.toMatchObject({ code: 'synthetic-failure' })
    else await expect(operation).resolves.toBe(true)
    expect(events.filter(event => event === 'plugin:ios-native|discard_file')).toHaveLength(1)
    expect(events.indexOf('plugin:ios-native|discard_file')).toBeGreaterThan(events.indexOf('native_file_job_status'))
    expect(appended).toEqual(state === 'succeeded' ? [{ id: 'ios-final-id', name: 'Synthetic' }] : [])
    expect(events).not.toContain('plugin:ios-native|pick_backup_source')
})

it('releases an unclaimed Android source when the native start command rejects', async () => {
    let owned = true
    const startFailure = new Error('Synthetic native worker busy')
    const dependencies: NativeFileJobDependencies = { isTauri: () => true, wait: async () => {}, invoke: async command => {
        expect(command).toBe('native_file_job_start'); throw startFailure
    } }
    await expect(importNativePresetFromPicker({ append: async () => { throw new Error('No append') }, importLegacy: async () => { throw new Error('No fallback') } }, {}, {
        isNative: () => true,
        chooseSource: async () => ({ source: { type: 'androidSpool', token: 'unclaimed-synthetic' }, displayName: 'synthetic.preset', releaseSource: async () => { owned = false } }),
        prepare: (source, name, options) => prepareNativePresetImport(source, name, options, dependencies),
    })).rejects.toBe(startFailure)
    expect(owned).toBe(false)
})
