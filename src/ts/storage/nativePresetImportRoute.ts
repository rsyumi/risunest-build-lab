import { isTauriDesktop, isTauriAndroid, isTauriIOS } from '../platform'
import { open } from './fileSelection'
import { pickAndroidPresetSource, discardAndroidSafSource } from './androidSafBridge'
import { pickIOSFile, discardIOSFile } from './iosFiles'
import { prepareNativePresetImport, type NativeFileJobOptions, type NativeFileJobSource, type PreparedNativePresetReceipt } from './nativeFileJobs'
import type { botPreset } from './database.svelte'

interface NativePresetImportDependencies {
    isNative(): boolean
    chooseSource(signal?: AbortSignal): Promise<{ source: NativeFileJobSource; displayName: string; releaseSource?(): Promise<unknown> } | null>
    discardSource?(source: NativeFileJobSource): Promise<unknown>
    prepare: typeof prepareNativePresetImport
}

const productionDependencies: NativePresetImportDependencies = {
    isNative: () => isTauriDesktop || isTauriAndroid || isTauriIOS,
    async chooseSource(signal) {
        if (isTauriAndroid) {
            const picked = await pickAndroidPresetSource({ signal })
            if (!picked) return null
            const source = picked.source
            if (source.type !== 'androidSpool' && source.type !== 'androidSeekable') throw new Error('Android preset source is invalid')
            return { ...picked, releaseSource: () => discardAndroidSafSource(source.token) }
        }
        if (isTauriIOS) {
            const picked = await pickIOSFile(signal)
            return picked ? {
                source: { type: 'desktopPath', path: picked.path }, displayName: picked.name,
                releaseSource: () => discardIOSFile(picked.path),
            } : null
        }
        const path = await open({ multiple: false, filters: [{ name: 'Preset', extensions: ['json', 'preset', 'risupreset', 'risup'] }] })
        return typeof path === 'string' ? { source: { type: 'desktopPath', path }, displayName: path.split(/[\\/]/).at(-1) || path } : null
    },
    async discardSource(source) {
        if (source.type === 'androidSpool' || source.type === 'androidSeekable') await discardAndroidSafSource(source.token)
    },
    prepare: prepareNativePresetImport,
}

export async function importPreparedNativePreset(
    receipt: PreparedNativePresetReceipt,
    actions: {
        append(preset: botPreset, signal?: AbortSignal): Promise<unknown>
        importLegacy(file: { name: string; data: Uint8Array }, signal?: AbortSignal): Promise<unknown>
    },
    options: NativeFileJobOptions = {},
): Promise<void> {
    try {
        options.signal?.throwIfAborted()
        if (receipt.nativeMapping.outcome === 'mapped') {
            await actions.append(receipt.nativeMapping.preset as unknown as botPreset, options.signal)
        } else {
            const data = await receipt.readSource()
            options.signal?.throwIfAborted()
            await actions.importLegacy({ name: receipt.displayName, data }, options.signal)
        }
    } finally {
        // Releasing a preparation never authorizes replay of a possibly committed append.
        await receipt.release().catch(error => console.error('Prepared preset cleanup failed', error))
    }
}

/** Returns false only on web, where the caller keeps the existing picker and import. */
export async function importNativePresetFromPicker(
    actions: Parameters<typeof importPreparedNativePreset>[1],
    options: NativeFileJobOptions = {},
    dependencies: NativePresetImportDependencies = productionDependencies,
): Promise<boolean> {
    if (!dependencies.isNative()) return false
    const selected = await dependencies.chooseSource(options.signal)
    if (!selected) return true
    let receipt: PreparedNativePresetReceipt
    try {
        if (options.signal?.aborted) {
            if (!selected.releaseSource) await dependencies.discardSource?.(selected.source)
            options.signal.throwIfAborted()
        }
        receipt = await dependencies.prepare(selected.source, selected.displayName, options)
    } finally {
        await selected.releaseSource?.().catch(error => console.error('Preset picker source cleanup failed', error))
    }
    await importPreparedNativePreset(receipt, actions, options)
    return true
}
