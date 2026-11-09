import { invoke } from '@tauri-apps/api/core'
import { isTauriDesktop, isTauriAndroid, isTauriIOS } from '../platform'
import { downloadDir, join } from '@tauri-apps/api/path'
import { normalizeDownloadFileName } from '../downloadFileName'
import { getPersistentDataRuntime } from './persistentDataRuntime.svelte'
import { readPersistentPresetBody } from './presetWorkingSetOperations'
import { NativeFileJobError, runNativePresetExport, type NativeFileJobOptions, type NativePresetExportInput } from './nativeFileJobs'
import { prepareNativeContentExportFromPicker, withNativeContentExportDestination, type NativeContentExportPickerDependencies } from './nativeContentExportPicker'
import type { botPreset } from './database.svelte'

interface NativePresetExportDependencies extends NativeContentExportPickerDependencies {
    chooseDestination(name: string, format: 'json' | 'risup'): Promise<string | null>
    readPreset(index: number, revision: number): Promise<botPreset>
    runExport: typeof runNativePresetExport
    stage(bytes: Uint8Array, signal?: AbortSignal): Promise<{ path: string; release(): Promise<void> }>
}

const productionDependencies: NativePresetExportDependencies = {
    isDesktop: () => isTauriDesktop,
    isAndroid: () => isTauriAndroid,
    isIOS: () => isTauriIOS,
    runtime: getPersistentDataRuntime,
    chooseDestination: async name => join(await downloadDir(), name),
    readPreset: (index, revision) => readPersistentPresetBody(getPersistentDataRuntime().store, revision, index),
    runExport: runNativePresetExport,
    async stage(bytes, signal) {
        signal?.throwIfAborted()
        const path = await invoke<string>('native_download_handoff_create')
        const release = () => invoke<void>('native_download_handoff_cleanup', { path })
        try {
            for (let offset = 0; offset < bytes.length; offset += 1024 * 1024) {
                signal?.throwIfAborted()
                const chunk = bytes.subarray(offset, offset + 1024 * 1024)
                const written = await invoke<number>('native_download_handoff_append', {
                    path, offset, chunk: Buffer.from(chunk).toString('base64'),
                })
                if (written !== offset + chunk.length) throw new Error('Preset handoff length differs from the written bytes')
            }
            signal?.throwIfAborted()
            return { path, release }
        } catch (error) {
            await release().catch(() => {})
            throw error
        }
    },
}

export function redactPresetForExport(preset: botPreset): botPreset {
    return { ...preset, openAIKey: '', forceReplaceUrl: '', forceReplaceUrl2: '', proxyKey: '', textgenWebUIStreamURL: '', textgenWebUIBlockingURL: '' }
}

export async function exportNativePresetFromPicker(
    input: {
        presetIndex: number
        preset: botPreset
        format: 'json' | 'risup'
        assertIdentity(): void
        encodeFallback(preset: botPreset): Promise<Uint8Array>
    },
    options: NativeFileJobOptions = {},
    dependencies: NativePresetExportDependencies = productionDependencies,
): Promise<{ data: botPreset; published: boolean } | undefined> {
    if (!dependencies.isDesktop() && !dependencies.isAndroid() && !dependencies.isIOS?.()) return undefined
    input.assertIdentity()
    const suggestedName = normalizeDownloadFileName(`${input.preset.name}_preset.${input.format}`)
    const flow = await prepareNativeContentExportFromPicker({
        suggestedName,
        flushReason: 'native-preset-export',
        chooseDestination: () => dependencies.chooseDestination(suggestedName, input.format),
    }, options, dependencies)
    if (flow.kind === 'unsupported') return undefined
    if (flow.kind === 'cancelled') return { data: redactPresetForExport(input.preset), published: false }
    let selectedData = redactPresetForExport(input.preset)
    return withNativeContentExportDestination(flow.destination, async () => {
        input.assertIdentity()
        const presetId = input.preset.id
        if (typeof presetId !== 'string' || !presetId) throw new Error('Native preset export requires a stable identity')
        const pinned = await dependencies.readPreset(input.presetIndex, flow.expectedRevision)
        input.assertIdentity()
        if (pinned.id !== presetId) throw new Error('Preset identity changed while pinning its revision')
        const data = redactPresetForExport(pinned)
        selectedData = data
        const request: NativePresetExportInput = {
            presetIndex: input.presetIndex, presetId, expectedRevision: flow.expectedRevision,
            destination: flow.destination, format: input.format,
        }
        try {
            await dependencies.runExport(request, options)
        } catch (error) {
            if (!(error instanceof NativeFileJobError) || error.code !== 'native-preset-export-declined') throw error
            input.assertIdentity()
            if (dependencies.runtime().revision !== flow.expectedRevision) throw new Error('Preset revision changed before compatibility export')
            const bytes = await input.encodeFallback(data)
            options.signal?.throwIfAborted()
            const staged = await dependencies.stage(bytes, options.signal)
            try {
                input.assertIdentity()
                if (dependencies.runtime().revision !== flow.expectedRevision) throw new Error('Preset revision changed before compatibility export')
                await dependencies.runExport({ ...request, compatibilitySourcePath: staged.path }, options)
            } finally {
                await staged.release().catch(error => console.error('Preset export cleanup failed', error))
            }
        }
        return { data, published: true }
    }).catch(error => {
        if (!options.signal?.aborted && error instanceof DOMException && error.name === 'AbortError') {
            return { data: selectedData, published: false }
        }
        throw error
    })
}
