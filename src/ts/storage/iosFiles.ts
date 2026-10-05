import { invoke } from '@tauri-apps/api/core'
import { join } from '@tauri-apps/api/path'
import { iosStagingPath } from './nativePaths'
import { mkdir, remove, writeFile } from '@tauri-apps/plugin-fs'
import { isTauriIOS } from '../platform'

export interface IOSPickedFile {
    path: string
    name: string
    bytes: number
}
export interface IOSPickedBackupSource { token: string; name: string; bytes: number }
interface InterruptedIOSBackupSources { names: (string | null)[]; cleanupFailed: boolean }
let interruptedSourceNotice: Promise<void> | undefined
/** Releases retired sources and finishes their notices before the next picker opens. */
export function reportInterruptedIOSBackupSources(): Promise<void> {
    if (interruptedSourceNotice) return interruptedSourceNotice
    const task = reportInterruptedSources()
    interruptedSourceNotice = task
    void task.finally(() => { if (interruptedSourceNotice === task) interruptedSourceNotice = undefined }).catch(() => {})
    return task
}
async function reportInterruptedSources(): Promise<void> {
    const interrupted = await invoke<InterruptedIOSBackupSources>('native_portable_source_cleanup_orphans')
    if (!interrupted || !Array.isArray(interrupted.names) || typeof interrupted.cleanupFailed !== 'boolean'
        || interrupted.names.some(name => name !== null && (typeof name !== 'string' || !name || /[\\/\r\n]/.test(name)))) {
        throw new Error('Interrupted backup source receipt is invalid')
    }
    if (interrupted.names.length) {
        const [{ alertError, waitAlert }, { failureReason }] = await Promise.all([
            import('../alert'), import('../gui/nativeFileJobDialogModel'),
        ])
        const reason = failureReason('import-interrupted')
        for (const name of interrupted.names) {
            alertError(name ? `${name}: ${reason}` : reason)
            await waitAlert()
        }
    }
    if (interrupted.cleanupFailed) throw new Error('Interrupted backup source cleanup failed')
}
/** On iOS, says at start when a reloaded page left a picked backup source unimported. */
export async function reportInterruptedIOSBackupSourcesAtStart(): Promise<void> {
    if (!isTauriIOS) return
    try {
        await reportInterruptedIOSBackupSources()
    } catch (error) {
        console.error('Interrupted iOS backup sources could not be released', error)
    }
}
export async function pickIOSBackupSource(signal?: AbortSignal): Promise<IOSPickedBackupSource | null> {
    if (signal?.aborted) throw cancelled()
    await reportInterruptedIOSBackupSources()
    if (signal?.aborted) throw cancelled()
    const selected = await invoke<IOSPickedBackupSource & { cancelled: boolean }>('plugin:ios-native|pick_backup_source')
    if (selected.cancelled) return null
    if (signal?.aborted) {
        if (!await discardIOSBackupSource(selected.token)) throw new Error('Backup source cleanup failed')
        throw cancelled()
    }
    return { token: selected.token, name: selected.name, bytes: selected.bytes }
}
export const discardIOSBackupSource = (token: string) => invoke<boolean>('native_portable_source_discard', { source: { type: 'iosScoped', token } })
export async function materializeIOSBackupSource(token: string): Promise<IOSPickedFile> {
    return invoke<IOSPickedFile>('plugin:ios-native|materialize_backup_source', { token })
}
const cancelled = () =>
    new DOMException('File operation cancelled', 'AbortError')

export async function pickIOSFile(
    signal?: AbortSignal,
): Promise<IOSPickedFile | null> {
    if (signal?.aborted) throw cancelled()
    const result = await invoke<IOSPickedFile & { cancelled: boolean }>(
        'plugin:ios-native|pick_file',
    )
    if (result.cancelled) return null
    if (signal?.aborted) {
        await discardIOSFile(result.path)
        throw cancelled()
    }
    return result
}

export const discardIOSFile = (path: string) =>
    invoke<void>('plugin:ios-native|discard_file', { path })

export async function exportIOSFile(request: {
    sourcePath: string
    suggestedName: string
    signal?: AbortSignal
    requestId?: string
}): Promise<{ bytes: number; warningCodes?: string[] }> {
    if (request.signal?.aborted) throw cancelled()
    const requestId = request.requestId ?? crypto.randomUUID()
    const result = await invoke<{ cancelled: boolean; bytes: number }>(
        'plugin:ios-native|export_file',
        {
            sourcePath: request.sourcePath,
            suggestedName: request.suggestedName,
            requestId,
        },
    )
    let cleanupFailed = false
    if (result.cancelled || !request.requestId) {
        await acknowledgeIOSPublication(requestId).catch(() => { cleanupFailed = true })
    }
    if (result.cancelled) throw cancelled()
    // Publication has completed. A late abort must not turn a published file into a reported failure.
    return { bytes: result.bytes, ...(cleanupFailed ? { warningCodes: ['cleanup-failed'] } : {}) }
}

export const acknowledgeIOSPublication = (id: string) =>
    invoke<void>('plugin:ios-native|acknowledge_publication', { id })

export async function getIOSPublication(
    requestId: string,
): Promise<{ bytes: number } | null> {
    const result = await invoke<{ state: string; bytes?: number }>(
        'plugin:ios-native|publication',
        { id: requestId },
    )
    return result.state === 'succeeded' && typeof result.bytes === 'number'
        ? { bytes: result.bytes }
        : null
}

export async function downloadIOSFile(
    name: string,
    bytes: Uint8Array,
): Promise<boolean> {
    const folder = await iosStagingPath()
    const path = await join(folder, 'export.bin')
    await mkdir(folder, { recursive: true })
    try {
        await writeFile(path, bytes)
        try {
            await exportIOSFile({ sourcePath: path, suggestedName: name })
            return true
        } catch (error) {
            if (error instanceof DOMException && error.name === 'AbortError')
                return false
            throw error
        }
    } finally {
        await remove(folder, { recursive: true })
    }
}
