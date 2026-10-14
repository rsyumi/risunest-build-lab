import { open as nativeOpen, save as nativeSave, type OpenDialogOptions, type SaveDialogOptions } from '@tauri-apps/plugin-dialog'
import { withLibraryFileSelection } from './libraryFileOperation'
import { isTauriMobile } from '../platform'

export const open = <T extends OpenDialogOptions>(options?: T) =>
    withLibraryFileSelection(() => nativeOpen(options))

export const save = (options?: SaveDialogOptions) =>
    withLibraryFileSelection(() => nativeSave(options))

export function selectDomFiles(options: { accept?: string; multiple?: boolean; signal?: AbortSignal }): Promise<File[]> {
    if (isTauriMobile) return import('./nativeFileSelection').then(({ selectNativeFiles }) => selectNativeFiles(options))
    return withLibraryFileSelection(() => new Promise((resolve, reject) => {
        if (options.signal?.aborted) { resolve([]); return }
        const input = document.createElement('input')
        input.type = 'file'
        input.accept = options.accept ?? ''
        input.multiple = options.multiple ?? false
        let settled = false
        const cleanup = () => {
            input.removeEventListener('change', change)
            input.removeEventListener('cancel', cancel)
            options.signal?.removeEventListener('abort', cancel)
            input.remove()
        }
        const finish = (files: File[]) => {
            if (settled) return
            settled = true
            cleanup()
            resolve(files)
        }
        const change = () => finish(Array.from(input.files ?? []))
        const cancel = () => finish([])
        input.addEventListener('change', change)
        input.addEventListener('cancel', cancel)
        options.signal?.addEventListener('abort', cancel, { once: true })
        document.body.appendChild(input)
        try { input.click(); input.style.display = 'none' }
        catch (error) { settled = true; cleanup(); reject(error) }
    }))
}
