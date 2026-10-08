import { language } from 'src/lang'

// Matched by name so the alert module does not load the save coordinator.
const internalSaveFailureNames = new Set([
    'WindowedConversationRequiresCompatibilityError',
    'WindowedConversationSaveError',
])

/** A save refusal whose own message describes internal state rather than what the user can do. */
export function isInternalSaveFailure(error: unknown): boolean {
    return error instanceof Error && internalSaveFailureNames.has(error.name)
}

/** The localized text for a failed local save. It never includes values or reasons from the error. */
export function localSaveFailureMessage(failure: unknown): string {
    const copy = language.risuNest.localSaveFailure
    const error = failure && typeof failure === 'object'
        ? failure as { code?: unknown; area?: unknown; name?: unknown; message?: unknown } : null
    const area = typeof error?.area === 'string' ? ({
        root: copy.settings, character: language.character, conversation: copy.conversation,
        presets: copy.presets, 'plugin storage': copy.plugins, 'asset aliases': copy.assets,
    } as Record<string, string>)[error.area] ?? copy.data : copy.data
    return error?.code === 'unsaveable-value'
        ? copy.invalid.replace('{0}', area)
        : error?.code === 'payload-too-large' ? copy.tooLarge
        : error?.name === 'QuotaExceededError' ? copy.storage
        : error?.name === 'WindowedConversationSaveError' ? copy.currentChat
        : error?.message === 'retired-record-id' ? copy.deleted : copy.failed
}
