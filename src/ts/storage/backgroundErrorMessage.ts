import { language } from 'src/lang'

/**
 * The alert text for a failure in background persistence work. Native commands reject with
 * plain `{ code, message? }` objects, sometimes as JSON text, so only their message is shown.
 */
export function backgroundErrorMessage(error: unknown): string | Error {
    if (error instanceof Error) return error
    let value: unknown = error
    if (typeof error === 'string') {
        try { value = JSON.parse(error) } catch { return error }
        if (!value || typeof value !== 'object') return error
    }
    const message = value && typeof value === 'object' ? (value as { message?: unknown }).message : undefined
    if (typeof message === 'string' && message.trim()) return message
    return language.risuNest.backgroundDataFailed
}
