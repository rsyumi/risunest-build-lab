import { language } from 'src/lang'

/**
 * Turns a native blocked-reason token (`backup-in-use`, `server-sync-busy`, ...) into the
 * sentence shown under a disabled backup or cleanup control. Unknown tokens get a generic
 * sentence so an internal identifier never reaches the screen.
 */
export function describeBlockedReason(reason: string | null | undefined): string | null {
    if (!reason) return null
    const known: Record<string, string> = language.risuNest.serverSync.management.blockedReasons
    return known[reason] ?? language.risuNest.serverSync.management.blockedReasonUnknown
}
