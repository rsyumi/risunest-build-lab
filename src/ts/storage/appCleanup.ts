import { invoke } from '@tauri-apps/api/core'

/** `remove-profile` is requested only from the desktop command line. */
export type AppCleanupMode = 'reset' | 'prepare-removal' | 'remove-profile'

export interface AppCleanupStatus {
    pending: boolean
    mode: AppCleanupMode | null
    error: string | null
    canCancel: boolean
    /** The launch profile this window runs, or null for the default profile. */
    profile: string | null
    /** Whether any named launch profile exists beside the default one. */
    namedProfiles: boolean
    cleanupFolder: string
}

function hasCode(error: unknown, code: string): boolean {
    return error === code || (error instanceof Error && error.message === code)
}

export function cleanupNeedsWebViewUpdate(error: unknown): boolean {
    return hasCode(error, 'cleanup-webview-update-required')
}

export function cleanupBlockedByOtherProfiles(error: unknown): boolean {
    return hasCode(error, 'cleanup-other-profiles-running')
}

export async function readAppCleanupStatus(): Promise<AppCleanupStatus> {
    return await invoke<AppCleanupStatus>('app_cleanup_status')
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
