import {isTauriDesktop, isTauriAndroid, isTauriIOS} from '../platform'
import {save} from '../storage/fileSelection'
import {runNativeContentExport, type NativeContentExportPickerDependencies} from '../storage/nativeContentExportPicker'
import type {NativeGalleryExportInput, NativeFileJobOptions, NativeFileJobResult} from '../storage/nativeFileJobs'
import {getBlobStore} from '../storage/platformBlobStore'
import type {GalleryItem} from './types'
import {RevisionConflictError} from '../storage/persistentDataStore'

export interface GalleryDownloadEntry {key: string; name: string; section: string}
export interface GalleryDownloadInput {
    kind: 'asset' | 'inlay'
    keys?: string[]
    entries?: GalleryDownloadEntry[]
    captureEntries?(revision: number): Promise<GalleryDownloadEntry[]>
    archive: boolean
    suggestedName: string
    item?: GalleryItem
}
export interface GalleryDownloadOptions {
    signal?: AbortSignal
    onProgress?(progress: {done: number; total: number}): void
}
export interface GalleryDownloadDependencies {
    isDesktop(): boolean
    isAndroid(): boolean
    isIOS(): boolean
    runtime(): {readonly revision: number; flushPendingData(reason: string): Promise<void>}
    reserveAndroidDestination?: NativeContentExportPickerDependencies['reserveAndroidDestination']
    chooseDestination(name: string): Promise<string | null>
    runExport(input: NativeGalleryExportInput, options: NativeFileJobOptions): Promise<NativeFileJobResult>
    downloadWeb(item: GalleryItem): Promise<void>
}

/** The native writer uses the same media suffix list when resolving leased aliases. */
export function galleryDownloadName(name: string, key: string, ext: string): string {
    const original = name.trim() && name !== key ? name : key
    const suffix = original.slice(original.lastIndexOf('.') + 1)
    const stem = original.includes('.') && suffix.toLowerCase() === ext.replace(/^\.+/, '').toLowerCase()
        ? original.slice(0, -(suffix.length + 1))
        : original.replace(/\.(jpg|jpeg|png|gif|webp|avif|wav|mp3|ogg|flac|webm|mp4|mkv)$/i, '')
    const encoder = new TextEncoder()
    const clean = (value: string, fallback: string, maxBytes: number) => {
        let cleaned = [...value.replace(/[\\/:*?"<>|\u0000-\u001f\u007f-\u009f]/g, '_').replace(/[. ]+$/, '')].slice(0, 120).join('').replace(/[. ]+$/, '') || fallback
        if (/^(CON|PRN|AUX|NUL|CONIN\$|CONOUT\$|COM[1-9¹²³]|LPT[1-9¹²³])$/i.test(cleaned.split('.')[0].replace(/ +$/, ''))) cleaned = `_${cleaned}`
        let bytes = 0
        let result = ''
        for (const character of cleaned) {
            bytes += encoder.encode(character).length
            if (bytes > maxBytes) break
            result += character
        }
        return result.replace(/[. ]+$/, '')
    }
    const extension = clean(ext.replace(/^\.+/, ''), 'bin', 120)
    return `${clean(stem, 'file', 255 - encoder.encode(extension).length - 1)}.${extension}`
}
/** The calendar date on this device, for default archive names. */
export function galleryArchiveDate(date: Date): string {
    return `${date.getFullYear()}-${String(date.getMonth() + 1).padStart(2, '0')}-${String(date.getDate()).padStart(2, '0')}`
}
async function productionDependencies(): Promise<GalleryDownloadDependencies> {
    const {getPersistentDataRuntime} = await import('../storage/persistentDataRuntime.svelte')
    const {runNativeGalleryExport} = await import('../storage/nativeFileJobs')
    return {
    isDesktop: () => isTauriDesktop,
    isAndroid: () => isTauriAndroid,
    isIOS: () => isTauriIOS,
    runtime: getPersistentDataRuntime,
    chooseDestination: name => save({defaultPath: name}),
    runExport: runNativeGalleryExport,
    async downloadWeb(item) {
        const data = await getBlobStore().read(item.key)
        if (!data) throw new Error('Gallery file is unavailable')
        const url = URL.createObjectURL(new Blob([data.slice().buffer as ArrayBuffer], {type: item.mime}))
        try {
            const link = document.createElement('a')
            link.href = url
            link.download = galleryDownloadName(item.name, item.key, item.ext)
            link.click()
        } finally { setTimeout(() => URL.revokeObjectURL(url), 1000) }
    },
}
}

export async function downloadGallery(input: GalleryDownloadInput, options: GalleryDownloadOptions = {},
    deps?: GalleryDownloadDependencies): Promise<{cancelled: boolean; skipped: number}> {
    deps ??= await productionDependencies()
    const cancelled = {cancelled: true, skipped: 0}
    try {
        if (options.signal?.aborted) return cancelled
        const suggestedName = !input.archive && input.item
            ? galleryDownloadName(input.item.name, input.item.key, input.item.ext) : input.suggestedName
        // The retry keeps the destination the user already picked.
        const downloaded = await runNativeContentExport({suggestedName, flushReason: 'gallery-download',
            androidSourceKind: 'download', chooseDestination: () => deps.chooseDestination(suggestedName)},
        {signal: options.signal}, deps, 'inline', async (flow, jobOptions) => {
            let revision = flow.expectedRevision
            for (let attempt = 0; attempt < 2; attempt++) {
                try {
                    const selection: NativeGalleryExportInput['selection'] = input.kind === 'asset'
                        ? {kind: 'asset', entries: input.captureEntries ? await input.captureEntries(revision) : input.entries ?? []}
                        : {kind: 'inlay', keys: input.keys ?? []}
                    const result = await deps.runExport({destination: flow.destination, expectedRevision: revision,
                        archive: input.archive, selection}, {signal: jobOptions.signal,
                        onStatus: status => {
                            jobOptions.onStatus?.(status)
                            options.onProgress?.({done: status.progress.completedItems, total: status.progress.totalItems ?? 0})
                        }})
                    const skipped = Number(result.warningCodes.find(code => code.startsWith('gallery-skipped:'))?.split(':')[1] ?? 0)
                    return {cancelled: false, skipped}
                } catch (error) {
                    const staleRevision = error instanceof RevisionConflictError ||
                        !!error && typeof error === 'object' && 'code' in error && error.code === 'revision-conflict'
                    if (attempt !== 0 || !staleRevision) throw error
                    await deps.runtime().flushPendingData('gallery-download-retry')
                    revision = deps.runtime().revision
                }
            }
            throw new Error('Gallery download failed')
        })
        if (downloaded === null) return cancelled
        if (downloaded === undefined) {
            if (input.archive || !input.item) throw new Error('Gallery download is unavailable')
            await deps.downloadWeb(input.item)
            return {cancelled: false, skipped: 0}
        }
        return downloaded
    } catch (error) {
        if (options.signal?.aborted || error instanceof Error && error.name === 'AbortError') return cancelled
        throw error
    }
}
