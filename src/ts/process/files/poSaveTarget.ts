import { writeFile } from '@tauri-apps/plugin-fs'
import { downloadFile } from 'src/ts/globalApi.svelte'
import { isTauri, isTauriIOS } from 'src/ts/platform'
import { save } from 'src/ts/storage/fileSelection'
import { pickRewritableIOSFile, rewriteIOSFile } from 'src/ts/storage/iosFiles'

export interface PoSaveTarget {
    /** Saves the result so far, where the destination can be rewritten. */
    checkpoint(result: string): Promise<void>
    finish(result: string): Promise<void>
}

/**
 * Asks for the destination once. The apps rewrite the chosen file after every
 * message, so a run that stops early keeps what it finished; the web saves the
 * final file through the usual download. Resolves null when the picker is
 * cancelled.
 */
export async function openPoSaveTarget(name: string): Promise<PoSaveTarget | null> {
    if (isTauriIOS) {
        if (!await pickRewritableIOSFile(name)) return null
        return { checkpoint: rewriteIOSFile, finish: rewriteIOSFile }
    }
    if (isTauri) {
        // On Android the picker returns a document URI, which the file plugin rewrites like a desktop path.
        const path = await save({ defaultPath: name })
        if (!path) return null
        const write = (result: string) => writeFile(path, new TextEncoder().encode(result))
        return { checkpoint: write, finish: write }
    }
    return {
        checkpoint: async () => {},
        async finish(result) { await downloadFile(name, result) },
    }
}
