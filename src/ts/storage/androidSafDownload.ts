import { invoke } from '@tauri-apps/api/core'
import {
    copyNativeExportToAndroidSaf,
    type AndroidSafDestinationRequest,
    type AndroidSafDestinationResult,
} from './androidSafBridge'

/** Raw bytes per append; the base64 text and its request stay within the 4 MiB plain request budget. */
export const ANDROID_DOWNLOAD_PIECE_BYTES = 3 * 1024 * 1024 - 3 * 1024

export interface AndroidSafDownloadDependencies {
    invoke<T>(command: string, args?: Record<string, unknown>): Promise<T>
    copy(request: AndroidSafDestinationRequest): Promise<AndroidSafDestinationResult>
}

const productionDependencies: AndroidSafDownloadDependencies = {
    invoke: (command, args) => invoke(command, args),
    copy: (request) => copyNativeExportToAndroidSaf(request),
}

/**
 * Saves a download through the Android picker without sending the file in one
 * request: the bytes go to an app-owned handoff in pieces, then Android copies
 * the handoff to the picked destination. Resolves false when the picker is
 * cancelled.
 */
export async function downloadThroughAndroidSaf(
    name: string,
    data: Uint8Array,
    dependencies: AndroidSafDownloadDependencies = productionDependencies,
): Promise<boolean> {
    const path = await dependencies.invoke<string>('native_download_handoff_create')
    try {
        for (let offset = 0; offset < data.length; offset += ANDROID_DOWNLOAD_PIECE_BYTES) {
            const piece = data.subarray(offset, offset + ANDROID_DOWNLOAD_PIECE_BYTES)
            const written = await dependencies.invoke<number>('native_download_handoff_append', {
                path,
                offset,
                chunk: Buffer.from(piece.buffer, piece.byteOffset, piece.byteLength).toString('base64'),
            })
            if (written !== offset + piece.length) {
                throw new Error('Download handoff length differs from the written bytes')
            }
        }
        let published: AndroidSafDestinationResult
        try {
            published = await dependencies.copy({ sourcePath: path, suggestedName: name })
        }
        catch (error) {
            if ((error as { name?: unknown } | null)?.name === 'AbortError') return false
            throw error
        }
        if (published.bytes !== data.length) {
            throw new Error('Android copied a different length than the download')
        }
        return true
    }
    finally {
        // Runs after the copy settles, so Android has already acknowledged it;
        // a handoff left behind is removed by the startup sweep.
        await dependencies.invoke('native_download_handoff_cleanup', { path }).catch((error) => {
            console.error('Download handoff cleanup failed', error)
        })
    }
}
