import { writeFile } from '@tauri-apps/plugin-fs'
import { downloadFile } from 'src/ts/globalApi.svelte'
import { isTauriAndroid, isTauriDesktop } from 'src/ts/platform'
import { save } from 'src/ts/storage/fileSelection'
import { publishAndroidSafDownload, reserveAndroidSafDownload } from 'src/ts/storage/androidSafDownload'

export interface PoSaveTarget {
    /** Saves the result so far, where the destination can be rewritten. */
    checkpoint(result: string): Promise<void>
    finish(result: string): Promise<void>
    /** Settles a destination that `finish` never wrote. */
    close(): Promise<void>
}

/**
 * Asks for the destination once. Desktop rewrites the chosen file after every
 * message and Android writes the destination picked up front when the run
 * ends; iOS and the web save the final file through the usual download.
 * Resolves null when the picker is cancelled.
 */
export async function openPoSaveTarget(name: string): Promise<PoSaveTarget | null> {
    const encode = (result: string) => new TextEncoder().encode(result)
    if (isTauriDesktop) {
        const path = await save({ defaultPath: name })
        if (!path) return null
        const write = (result: string) => writeFile(path, encode(result))
        return { checkpoint: write, finish: write, close: async () => {} }
    }
    if (isTauriAndroid) {
        const reservation = await reserveAndroidSafDownload(name)
        if (!reservation) return null
        // The picked file takes a single write, so a run that stops early writes its last checkpoint.
        let checkpoint: string | null = null
        let used = false
        return {
            checkpoint: async (result) => { checkpoint = result },
            async finish(result) {
                used = true
                await publishAndroidSafDownload(reservation, name, encode(result))
            },
            async close() {
                if (used) return
                used = true
                const closing = checkpoint === null
                    ? reservation.release()
                    : publishAndroidSafDownload(reservation, name, encode(checkpoint))
                await closing.catch((error) => {
                    console.error('PO save destination could not be closed', error)
                })
            },
        }
    }
    return {
        checkpoint: async () => {},
        async finish(result) { await downloadFile(name, result) },
        close: async () => {},
    }
}
