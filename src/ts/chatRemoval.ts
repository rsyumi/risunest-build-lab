import {
    requireCurrentConversationSession,
    type ActiveConversationSession,
} from './storage/activeConversationSession'
import type { Chat, Database } from './storage/database.svelte'
import {
    captureChatMessageTarget,
    resolveChatMessageTarget,
    type CapturedChatMessageTarget,
} from './chatMessageUi'

export interface CurrentChatRemovalTarget {
    character: Database['characters'][number]
    conversation: Chat
}

export interface RemoveChatMessageOptions {
    absoluteIndex: number
    captureTarget?: () => CapturedChatMessageTarget | null
    shiftKey: boolean
    recursive: boolean
    askRemoval: boolean
    instantRemove: boolean
    captureCurrent(): CurrentChatRemovalTarget | null
    getCurrentSession(): ActiveConversationSession | null
    mutationBlocked?(): boolean
    confirmRemoval(): Promise<boolean>
    confirmInstantRemoval(): Promise<{ confirmed: boolean; checked: boolean }>
}

export async function removeChatMessage(options: RemoveChatMessageOptions): Promise<'removed' | 'cancelled' | 'stale' | 'blocked'> {
    const target = options.captureTarget
        ? options.captureTarget()
        : captureChatMessageTarget({
            absoluteIndex: options.absoluteIndex,
            captureCurrent: options.captureCurrent,
            getCurrentSession: options.getCurrentSession,
        })
    if (!target) return 'stale'

    if (options.shiftKey) return mutateCapturedTarget(options, target, 'truncate')

    if (options.instantRemove || options.recursive) {
        const result = await options.confirmInstantRemoval()
        if (!result.confirmed) return 'cancelled'
        if (!isCurrentTarget(options, target)) return 'stale'
        return mutateCapturedTarget(options, target, result.checked ? 'delete' : 'truncate')
    }
    if (options.askRemoval && !await options.confirmRemoval()) return 'cancelled'
    if (!isCurrentTarget(options, target)) return 'stale'
    return mutateCapturedTarget(options, target, 'delete')
}

function isCurrentTarget(
    options: RemoveChatMessageOptions,
    target: CapturedChatMessageTarget,
): boolean {
    return resolveChatMessageTarget(target, options) !== null
}

function mutateCapturedTarget(
    options: RemoveChatMessageOptions,
    target: CapturedChatMessageTarget,
    mutation: 'delete' | 'truncate',
): 'removed' | 'stale' | 'blocked' {
    if (options.mutationBlocked?.()) return 'blocked'
    const current = resolveChatMessageTarget(target, options)
    if (!current) return 'stale'
    if (current.kind === 'session') {
        const session = requireCurrentConversationSession(
            current.session,
            options.getCurrentSession(),
        )
        if (mutation === 'delete') session.delete(current.locator)
        else session.truncate(current.locator)
        return 'removed'
    }
    if (mutation === 'delete') {
        current.conversation.message.splice(current.absoluteIndex, 1)
        current.conversation.message = current.conversation.message
    } else {
        current.conversation.message = current.conversation.message.slice(
            0,
            current.absoluteIndex,
        )
    }
    return 'removed'
}
