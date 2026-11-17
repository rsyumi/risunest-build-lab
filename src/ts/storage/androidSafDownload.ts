import { invoke } from '@tauri-apps/api/core'
import {
    copyNativeExportToAndroidSaf,
    reserveAndroidSafDestination,
    bindAndroidSafDestinationSource,
    withAndroidSafDestination,
    type AndroidSafDestinationRequest,
    type AndroidSafDestinationReservation,
    type AndroidSafDestinationResult,
} from './androidSafBridge'
import { runWithMobileBackgroundTask } from '../mobileBackgroundTask'

/** Raw bytes per append; the base64 text and its request stay within the 4 MiB plain request budget. */
export const ANDROID_DOWNLOAD_PIECE_BYTES = 3 * 1024 * 1024 - 3 * 1024

export interface AndroidSafDownloadDependencies {
    reserve: typeof reserveAndroidSafDestination
    invoke<T>(command: string, args?: Record<string, unknown>): Promise<T>
    copy(request: AndroidSafDestinationRequest): Promise<AndroidSafDestinationResult>
}

const productionDependencies: AndroidSafDownloadDependencies = {
    invoke: (command, args) => invoke(command, args),
    copy: (request) => copyNativeExportToAndroidSaf(request),
    reserve: reserveAndroidSafDestination,
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
    const reservation = await reserveAndroidSafDownload(name, dependencies)
    return reservation ? publishAndroidSafDownload(reservation, name, data, dependencies) : false
}

/** Opens the Android picker for a download written later. Resolves null when the picker is cancelled. */
async function reserveAndroidSafDownload(
    name: string,
    dependencies: AndroidSafDownloadDependencies = productionDependencies,
): Promise<AndroidSafDestinationReservation | null> {
    try { return await dependencies.reserve(name, 'download') }
    catch (error) {
        if ((error as {name?: string})?.name === 'AbortError') return null
        throw error
    }
}

/** Writes a download to a reserved destination and releases the reservation. */
function publishAndroidSafDownload(
    reservation: AndroidSafDestinationReservation,
    name: string,
    data: Uint8Array,
    dependencies: AndroidSafDownloadDependencies = productionDependencies,
): Promise<boolean> {
    // The destination is already chosen, so the picker never holds the background task.
    return runWithMobileBackgroundTask('export', () => writeAndroidSafDownload(reservation, name, data, dependencies), undefined, true)
}

async function writeAndroidSafDownload(
    reservation: AndroidSafDestinationReservation,
    name: string,
    data: Uint8Array,
    dependencies: AndroidSafDownloadDependencies,
): Promise<boolean> {
    let path: string | undefined
    try {
        return await withAndroidSafDestination(reservation, async () => {
            path = await dependencies.invoke<string>('native_download_handoff_create')
            await bindAndroidSafDestinationSource(reservation, path)
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
            const published = await dependencies.copy({ sourcePath: path, suggestedName: name, reservation })
            if (published.bytes !== data.length) {
                throw new Error('Android copied a different length than the download')
            }
            return true
        })
    }
    catch (error) {
        if ((error as { name?: unknown } | null)?.name === 'AbortError') return false
        throw error
    }
    finally {
        // Runs after the copy settles, so Android has already acknowledged it;
        // a handoff left behind is removed by the startup sweep.
        if (path) await dependencies.invoke('native_download_handoff_cleanup', { path }).catch((error) => {
            console.error('Download handoff cleanup failed', error)
        })
    }
}
