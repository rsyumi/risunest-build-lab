import { describe, expect, it, vi } from 'vitest'
const native = vi.hoisted(() => ({ invoke: vi.fn() }))
vi.mock('@tauri-apps/api/core', async original => ({ ...await original<typeof import('@tauri-apps/api/core')>(), invoke: native.invoke } satisfies typeof import('@tauri-apps/api/core')))
vi.mock('../platform', async () => (await import('../platform.testSupport')).platformModule({ runtime: 'tauri', os: 'windows' }))
vi.mock('./persistentDataRuntime.svelte', () => ({ getPersistentDataRuntime: vi.fn() }))
import { BaseDirectory } from '@tauri-apps/api/path'
import { getPersistentDataRuntime } from './persistentDataRuntime.svelte'
import { createPersistentStoreHarness } from '../../../tests/support/persistentStoreHarness'
import { exportNativePresetFromPicker } from './nativePresetExportRoute'
import { NativeFileJobError, runNativePresetExport } from './nativeFileJobs'
import { androidSafReservationFixture } from '../../../tests/support/androidSafReservation'
import type { botPreset } from './database.svelte'
import type { NativePresetExportInput, NativeFileJobResult } from './nativeFileJobs'

const preset = { id: 'stable-preset', name: 'Synthetic', mainPrompt: 'body', openAIKey: 'secret', messageNameMode: 'local', opaque: { retained: true } } as unknown as botPreset
const result: NativeFileJobResult = { revision: 8, sourceBytes: 2, sourceSha256: 'a'.repeat(64), sourceFingerprintKind: 'whole-file-sha256', characterCount: 0, presetCount: 1, warningCodes: [] }

function setup() {
    let revision = 8
    let valid = true
    let declined = false
    const calls: string[] = []
    const requests: NativePresetExportInput[] = []
    const input = {
        presetIndex: 1, preset, format: 'risup' as const,
        assertIdentity: () => { if (!valid) throw new Error('Preset list changed') },
        encodeFallback: async (data: botPreset) => { calls.push('encode'); expect(data.openAIKey).toBe(''); return Uint8Array.of(9, 8, 7) },
    }
    const dependencies = {
        isDesktop: () => true, isAndroid: () => false,
        chooseDestination: async () => { calls.push('picker'); return 'synthetic.risup' as string | null },
        runtime: () => ({ get revision() { return revision }, flushPendingData: async () => { calls.push('flush') } }),
        readPreset: async (index: number, pinned: number) => { expect([index, pinned]).toEqual([1, 8]); calls.push('read'); return structuredClone(preset) },
        runExport: async (request: NativePresetExportInput) => { requests.push(request); calls.push('export'); if (declined && !request.compatibilitySourcePath) throw new NativeFileJobError('native-preset-export-declined', 'declined'); return result },
        stage: async (bytes: Uint8Array) => { expect(bytes).toEqual(Uint8Array.of(9, 8, 7)); calls.push('stage'); return { path: 'owned-download', release: async () => { calls.push('release') } } },
    }
    return { input, dependencies, calls, requests, decline: () => { declined = true }, change: () => { valid = false }, revise: () => { revision++ } }
}

describe('native preset export route', () => {
    it('pins exact identity/index and returns the secret-blanked local record', async () => {
        const h = setup()
        const output = await exportNativePresetFromPicker(h.input, {}, h.dependencies)
        expect(h.requests).toEqual([{ presetIndex: 1, presetId: 'stable-preset', format: 'risup', expectedRevision: 8, destination: { type: 'desktopPath', path: 'synthetic.risup' } }])
        expect(h.calls).toEqual(['picker', 'flush', 'read', 'export'])
        expect(output).toEqual({ published: true, data: { ...preset, openAIKey: '', forceReplaceUrl: '', forceReplaceUrl2: '', proxyKey: '', textgenWebUIStreamURL: '', textgenWebUIBlockingURL: '' } })
        expect(preset.openAIKey).toBe('secret')
    })

    it('stages exact TS bytes on decline and reuses the destination and pinned identity once', async () => {
        const h = setup(); h.decline()
        await expect(exportNativePresetFromPicker(h.input, {}, h.dependencies)).resolves.toMatchObject({ published: true })
        expect(h.calls).toEqual(['picker', 'flush', 'read', 'export', 'encode', 'stage', 'export', 'release'])
        expect(h.requests[1]).toEqual({ ...h.requests[0], compatibilitySourcePath: 'owned-download' })
    })

    it.each(['picker', 'flush', 'read'] as const)('rejects a list change during %s before native publication', async during => {
        const h = setup()
        if (during === 'picker') h.dependencies.chooseDestination = async () => { h.change(); return 'synthetic.risup' }
        if (during === 'flush') h.dependencies.runtime = () => ({ revision: 8, flushPendingData: async () => { h.change() } })
        if (during === 'read') h.dependencies.readPreset = async () => { h.change(); return preset }
        await expect(exportNativePresetFromPicker(h.input, {}, h.dependencies)).rejects.toThrow('Preset list changed')
        expect(h.requests).toEqual([])
    })

    it('rejects a replaced identity and a revision change while fallback bytes are prepared', async () => {
        const h = setup()
        h.dependencies.readPreset = async () => ({ ...preset, id: 'replacement' })
        await expect(exportNativePresetFromPicker(h.input, {}, h.dependencies)).rejects.toThrow('identity changed')
        expect(h.requests).toEqual([])
        const declined = setup(); declined.decline()
        declined.input.encodeFallback = async () => { declined.revise(); return Uint8Array.of(9, 8, 7) }
        await expect(exportNativePresetFromPicker(declined.input, {}, declined.dependencies)).rejects.toThrow('revision changed')
        expect(declined.requests).toHaveLength(1)
        expect(declined.calls.at(-1)).toBe('release')
    })

    it('keeps web on TS and reports picker cancellation without publication', async () => {
        const h = setup(); h.dependencies.isDesktop = () => false
        await expect(exportNativePresetFromPicker(h.input, {}, h.dependencies)).resolves.toBeUndefined()
        expect(h.calls).toEqual([])
        h.dependencies.isDesktop = () => true
        h.dependencies.chooseDestination = async () => null
        await expect(exportNativePresetFromPicker(h.input, {}, h.dependencies)).resolves.toMatchObject({ published: false, data: { id: 'stable-preset', openAIKey: '' } })
        expect(h.requests).toEqual([])
    })
})

it('reports mobile publication cancellation without a success result and preserves explicit aborts', async () => {
    const h = setup()
    h.dependencies.runExport = async () => { throw new DOMException('Picker cancelled', 'AbortError') }
    await expect(exportNativePresetFromPicker(h.input, {}, h.dependencies)).resolves.toMatchObject({ published: false, data: { id: 'stable-preset' } })
    const abort = new AbortController()
    h.dependencies.runExport = async () => { abort.abort(); throw new DOMException('Cancelled', 'AbortError') }
    await expect(exportNativePresetFromPicker(h.input, { signal: abort.signal }, h.dependencies)).rejects.toMatchObject({ name: 'AbortError' })
})

it('binds one real Android reservation only after the successful fallback job is prepared', async () => {
    const h = setup()
    const picker = androidSafReservationFixture()
    let boundId: string | undefined
    const bound: string[] = []
    picker.bridge.bindExport.mockImplementation(async (_requestId, exportId) => {
        if (boundId && boundId !== exportId) return false
        boundId = exportId; bound.push(exportId); return true
    })
    let latestJob = ''
    let starts = 0
    let copies = 0
    const associations: unknown[] = []
    const jobDependencies = {
        isTauri: () => true, wait: async () => {},
        invoke: async (command: string, args?: Record<string, unknown>) => {
            if (command === 'native_file_job_start') {
                const request = args!.request as Record<string, unknown>
                latestJob = ++starts === 1 ? 'declined-job' : 'fallback-job'
                expect(request.compatibilitySourcePath).toBe(starts === 1 ? undefined : 'owned-download')
                return { jobId: latestJob }
            }
            if (command === 'native_file_job_status') return {
                jobId: latestJob, kind: 'export-preset', state: latestJob === 'declined-job' ? 'failed' : 'succeeded', phase: 'complete', progress: { completedBytes: 0, completedItems: 0 },
                ...(latestJob === 'declined-job' ? { error: { code: 'native-preset-export-declined', message: 'declined' } } : { result: { ...result, handoffPath: 'preset-123e4567-e89b-42d3-a456-426614174004.risup' } }),
            }
            if (command === 'native_file_job_forget' || command === 'native_preset_handoff_cleanup') return true
            throw new Error(command)
        },
        copyToAndroidSaf: async (request: import('./androidSafBridge').AndroidSafDestinationRequest) => {
            expect(request.reservation?.jobId).toBe('fallback-job')
            expect(request.reservation?.requestId).toBe(picker.bridge.reserveExport.mock.calls[0][0])
            copies++; return { bytes: result.sourceBytes, warningCodes: [] }
        },
    }
    const dependencies = {
        ...h.dependencies, isDesktop: () => false, isAndroid: () => true, reserveAndroidDestination: picker.reserve,
        runExport: (input: NativePresetExportInput, options?: import('./nativeFileJobs').NativeFileJobOptions) => runNativePresetExport(input, options, jobDependencies),
    }
    await expect(exportNativePresetFromPicker(h.input, {
        onStarted: (jobId, destinationRequestId) => { expect(boundId).toBe(jobId); associations.push([jobId, destinationRequestId]) },
    }, dependencies)).resolves.toMatchObject({ published: true })
    expect(starts).toBe(2)
    expect(copies).toBe(1)
    expect(bound).toEqual(['fallback-job'])
    expect(picker.bridge.reserveExport).toHaveBeenCalledOnce()
    expect(associations).toEqual([['fallback-job', picker.bridge.reserveExport.mock.calls[0][0]]])
})

it('uses the production Downloads destination and normalized filename without opening a desktop dialog', async () => {
    const selected = { ...preset, name: 'A/B:Name' }
    const storage = await createPersistentStoreHarness({ characters: [], botPresets: [selected], botPresetsId: 0 } as never)
    vi.mocked(getPersistentDataRuntime).mockReturnValue({ revision: storage.revision, store: storage.store, flushPendingData: async () => {} } as never)
    const commands: string[] = []
    const destination = 'C:/synthetic/Downloads/A_B_Name_preset.risup'
    native.invoke.mockImplementation(async (command: string, args?: Record<string, unknown>) => {
        commands.push(command)
        if (command === 'plugin:path|resolve_directory') { expect(args).toEqual({ directory: BaseDirectory.Download }); return 'C:/synthetic/Downloads' }
        if (command === 'plugin:path|join') { expect(args).toEqual({ paths: ['C:/synthetic/Downloads', 'A_B_Name_preset.risup'] }); return destination }
        if (command === 'native_file_job_start') {
            expect(args).toEqual({ request: { kind: 'export-preset', expectedRevision: storage.revision, presetIndex: 0, presetId: selected.id, format: 'risup', destination } })
            return { jobId: 'desktop-preset' }
        }
        if (command === 'native_file_job_status') return { jobId: 'desktop-preset', kind: 'export-preset', state: 'succeeded', phase: 'complete', progress: { completedBytes: 2, completedItems: 1 }, result: { ...result, revision: storage.revision } }
        if (command === 'native_file_job_forget') return true
        throw new Error(`No desktop picker is allowed: ${command}`)
    })
    vi.stubGlobal('window', Object.assign(new EventTarget(), { __TAURI_INTERNALS__: { invoke: native.invoke } }))
    try {
        await expect(exportNativePresetFromPicker({ presetIndex: 0, preset: selected, format: 'risup', assertIdentity: () => {}, encodeFallback: async () => { throw new Error('No fallback') } })).resolves.toMatchObject({ published: true })
        expect(commands).toEqual(['plugin:path|resolve_directory', 'plugin:path|join', 'native_file_job_start', 'native_file_job_status', 'native_file_job_forget'])
    } finally { vi.unstubAllGlobals() }
})

it('normalizes the suggested mobile filename before reserving its destination', async () => {
    const h = setup()
    const picker = androidSafReservationFixture()
    let destination: NativePresetExportInput['destination'] | undefined
    await exportNativePresetFromPicker({ ...h.input, preset: { ...preset, name: 'A/B:Name' } }, {}, {
        ...h.dependencies, isDesktop: () => false, isAndroid: () => true, reserveAndroidDestination: picker.reserve,
        runExport: async request => { destination = request.destination; return result },
    })
    expect(picker.bridge.reserveExport.mock.calls[0][1]).toBe('A_B_Name_preset.risup')
    expect(destination).toMatchObject({ type: 'androidSaf', suggestedName: 'A_B_Name_preset.risup' })
})
