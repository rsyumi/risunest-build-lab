import { downloadDir, join } from '@tauri-apps/api/path'

import { isTauriIOS, isTauriAndroid, isTauriDesktop } from '../platform'
import {
    runNativeDatasetExport,
    NativeFileJobError,
    type NativeDatasetExportInput,
    type NativeFileJobOptions,
    type NativeFileJobResult,
} from './nativeFileJobs'
import { getPersistentDataRuntime } from './persistentDataRuntime.svelte'
import {
    prepareNativeContentExportFromPicker,
    type NativeContentExportPickerDependencies,
} from './nativeContentExportPicker'
import { NativeFileOperationBusyError, runSharedNativeFileOperation } from './nativeFileJobManager'

export const DATASET_EXPORT_FILE_NAME = 'dataset.json'

interface NativeDatasetExportRouteDependencies
    extends NativeContentExportPickerDependencies {
    desktopDestination(fileName: string): Promise<string>
    runExport(
        input: NativeDatasetExportInput,
        options?: NativeFileJobOptions,
    ): Promise<NativeFileJobResult>
}

const productionDependencies: NativeDatasetExportRouteDependencies = {
    isDesktop: () => isTauriDesktop,
    isAndroid: () => isTauriAndroid,
    isIOS: () => isTauriIOS,
    runtime: getPersistentDataRuntime,
    // The desktop export lands in Downloads without a dialog, as the
    // renderer download did.
    desktopDestination: async (fileName) => join(await downloadDir(), fileName),
    runExport: runNativeDatasetExport,
}

/**
 * Streams the dataset from a leased native store revision.
 * `undefined` selects the renderer implementation. Native outcomes are shown
 * by the shared operation dialog; `null` means it displayed a failure or cancellation.
 */
export async function exportNativeDataset(
    options: NativeFileJobOptions = {},
    dependencies: NativeDatasetExportRouteDependencies = productionDependencies,
): Promise<NativeFileJobResult | null | undefined> {
    if (!dependencies.isDesktop() && !dependencies.isAndroid() && !dependencies.isIOS?.()) return undefined
    try {
        return await runSharedNativeFileOperation('export', 'dataset-export', async context => {
            const controller = new AbortController()
            const signals = [context.signal, ...(options.signal ? [options.signal] : [])]
            const abort = () => controller.abort()
            for (const source of signals) {
                if (source.aborted) abort()
                else source.addEventListener('abort', abort, { once: true })
            }
            try {
                const sharedOptions = { ...options, signal: controller.signal,
                    onStatus: (status: Parameters<typeof context.onStatus>[0]) => {
                        context.onStatus(status)
                        options.onStatus?.(status)
                    } }
                controller.signal.throwIfAborted()
                const flow = await prepareNativeContentExportFromPicker({
                    suggestedName: DATASET_EXPORT_FILE_NAME,
                    flushReason: 'native-dataset-export',
                    chooseDestination: () => dependencies.desktopDestination(DATASET_EXPORT_FILE_NAME),
                }, sharedOptions, dependencies)
                if (flow.kind === 'unsupported') throw new Error('Native dataset export is unavailable')
                if (flow.kind === 'cancelled') throw new Error('The Downloads folder is unavailable')
                return await dependencies.runExport({
                    destination: flow.destination,
                    expectedRevision: flow.expectedRevision,
                }, sharedOptions)
            } finally {
                for (const source of signals) source.removeEventListener('abort', abort)
            }
        }, { format: 'dataset', presentation: 'dialog' })
    } catch (error) {
        if (error instanceof NativeFileOperationBusyError ||
            error instanceof NativeFileJobError && error.code === 'generation-active') throw error
        return null
    }
}
