import { invoke } from '@tauri-apps/api/core'
import { open } from '@tauri-apps/plugin-fs'
import { isTauriAndroid, isTauriIOS } from '../platform'
import { withLibraryFileSelection } from './libraryFileOperation'

export interface NativeSelectedFile {
    path: string
    name: string
    bytes: number
    mimeType: string
}

export interface NativeFileSelectionBridge {
    pick(multiple: boolean): Promise<{ cancelled: boolean; files?: NativeSelectedFile[] }>
    discard(path: string): Promise<void>
}

export interface FileSelectionOptions {
    accept?: string
    multiple?: boolean
    signal?: AbortSignal
}

let selecting = false
const READ_CHUNK_BYTES = 256 * 1024

function bridge(): NativeFileSelectionBridge {
    if (isTauriAndroid) {
        if (!window.RisuNativeFileSelection) throw new Error('Android native file selection is unavailable')
        return window.RisuNativeFileSelection
    }
    if (isTauriIOS) return {
        pick: multiple => invoke('plugin:ios-native|pick_files', { multiple }),
        discard: path => invoke('plugin:ios-native|discard_file', { path }),
    }
    throw new Error('Native mobile file selection is unavailable')
}

function accepts(file: NativeSelectedFile, accept = ''): boolean {
    const types = accept.toLowerCase().split(',').map(type => type.trim()).filter(Boolean)
    return !types.length || types.some(type => type === '*' || type === '*/*'
        || (type.startsWith('.') ? file.name.toLowerCase().endsWith(type)
            : type.endsWith('/*') ? file.mimeType.toLowerCase().startsWith(type.slice(0, -1))
                : file.mimeType.toLowerCase() === type))
}

async function readSelected(file: NativeSelectedFile, signal?: AbortSignal): Promise<Uint8Array<ArrayBuffer>> {
    signal?.throwIfAborted()
    if (!Number.isSafeInteger(file.bytes) || file.bytes < 0) throw new Error('The selected file has an invalid size')
    const handle = await open(file.path, { read: true })
    let failure: unknown
    try {
        const data = new Uint8Array(file.bytes)
        let position = 0
        while (position < data.length) {
            signal?.throwIfAborted()
            const count = await handle.read(data.subarray(position, Math.min(position + READ_CHUNK_BYTES, data.length)))
            if (!count) throw new Error('The selected file ended before all bytes were read')
            position += count
        }
        signal?.throwIfAborted()
        if (await handle.read(new Uint8Array(1))) throw new Error('The selected file changed while reading')
        return data
    } catch (error) {
        failure = error
        throw error
    } finally {
        try { await handle.close() }
        catch (error) {
            throw new Error([failure, error].filter(Boolean).map(String).join('\n'), { cause: failure ?? error })
        }
    }
}

export function selectNativeFileData(options: FileSelectionOptions = {}) {
    return withLibraryFileSelection(async () => {
        if (options.signal?.aborted) return []
        if (selecting) throw new Error('A file picker is already open')
        const native = bridge()
        selecting = true
        let files: NativeSelectedFile[] = []
        let failure: unknown
        try {
            const result = await native.pick(options.multiple ?? false)
            if (result.cancelled) return []
            if (!Array.isArray(result.files) || !result.files.length) throw new Error('The file picker returned no document')
            files = result.files
            if (options.signal?.aborted) return []
            const selected: { name: string; data: Uint8Array<ArrayBuffer>; mimeType: string }[] = []
            for (const file of options.multiple ? files : files.slice(0, 1)) {
                if (!accepts(file, options.accept)) continue
                selected.push({ name: file.name, data: await readSelected(file, options.signal), mimeType: file.mimeType })
            }
            return selected
        } catch (error) {
            failure = error
            throw error
        } finally {
            const errors: unknown[] = []
            for (const file of files) {
                try { await native.discard(file.path) }
                catch (error) { errors.push(error) }
            }
            selecting = false
            if (errors.length) throw new Error([failure, ...errors].filter(Boolean).map(String).join('\n'), { cause: failure ?? errors[0] })
        }
    })
}

export async function selectNativeFiles(options: FileSelectionOptions): Promise<File[]> {
    return (await selectNativeFileData(options)).map(file => new File([file.data], file.name, { type: file.mimeType }))
}
