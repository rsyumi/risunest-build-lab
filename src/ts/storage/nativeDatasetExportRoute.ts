import { downloadDir, join } from '@tauri-apps/api/path'

import { isTauriIOS, isTauriAndroid, isTauriDesktop } from '../platform'
import {
    runNativeDatasetExport,
    type NativeDatasetExportInput,
    type NativeFileJobOptions,
    type NativeFileJobResult,
} from './nativeFileJobs'
import { getPersistentDataRuntime } from './persistentDataRuntime.svelte'
import {
    prepareNativeContentExportFromPicker,
    type NativeContentExportPickerDependencies,
} from './nativeContentExportPicker'

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
 * Streams the dataset from a leased native store revision. Resolves
 * `undefined` when the platform has no native store export, so the caller
 * keeps the renderer implementation.
 */
export async function exportNativeDataset(
    options: NativeFileJobOptions = {},
    dependencies: NativeDatasetExportRouteDependencies = productionDependencies,
): Promise<NativeFileJobResult | undefined> {
    const flow = await prepareNativeContentExportFromPicker({
        suggestedName: DATASET_EXPORT_FILE_NAME,
        flushReason: 'native-dataset-export',
        chooseDestination: () =>
            dependencies.desktopDestination(DATASET_EXPORT_FILE_NAME),
    }, options, dependencies)
    if (flow.kind === 'unsupported') return undefined
    if (flow.kind === 'cancelled') {
        throw new Error('The Downloads folder is unavailable')
    }
    return dependencies.runExport({
        destination: flow.destination,
        expectedRevision: flow.expectedRevision,
    }, options)
}
