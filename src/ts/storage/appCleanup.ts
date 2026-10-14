import { invoke } from '@tauri-apps/api/core'

export type AppCleanupMode = 'reset' | 'prepare-removal'

export function cleanupNeedsWebViewUpdate(error: unknown): boolean {
    return error === 'cleanup-webview-update-required'
        || (error instanceof Error && error.message === 'cleanup-webview-update-required')
}

export async function requestAppCleanup(options: {
    native: boolean
    desktop: boolean
    prepareRemoval: boolean
    confirm: () => Promise<boolean>
}): Promise<boolean> {
    if (!options.native) return false
    if (options.prepareRemoval && !options.desktop) throw new Error('app-cleanup-mode-unavailable')
    if (!await options.confirm()) return false
    const mode: AppCleanupMode = options.prepareRemoval ? 'prepare-removal' : 'reset'
    await invoke('app_cleanup_request', { mode })
    return true
}
