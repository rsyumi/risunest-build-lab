import type { Message } from '../storage/database.svelte'
import type { MenuDef } from '../stores.svelte'

export type MessageButtonRole = 'user' | 'char'

export interface MessageButtonDef extends MenuDef {
    /** Omitted: both roles. */
    roles?: MessageButtonRole[]
}

export interface MessageButtonTarget {
    characterIndex: number
    chatIndex: number
    /** Absolute stored index, also in a windowed conversation. */
    messageIndex: number
    messageId: string | null
    role: MessageButtonRole
    characterId: string
    conversationId: string
}

export const additionalMessageButtons = $state([] as MessageButtonDef[])

export function normalizeMessageButtonRoles(roles: unknown): MessageButtonRole[] | undefined {
    if (roles === undefined) return undefined
    if (!Array.isArray(roles) || !roles.length || roles.some((role) => role !== 'user' && role !== 'char')) {
        throw new Error("registerButton: options.roles must be a non-empty array of 'user' and 'char'")
    }
    return [...new Set(roles as MessageButtonRole[])]
}

export function messageButtonsForRole(role: unknown): MessageButtonDef[] {
    if (role !== 'user' && role !== 'char') return []
    return additionalMessageButtons.filter((button) => !button.roles || button.roles.includes(role))
}

export function createMessageButtonTarget(input: {
    characterIndex: number
    character: { chaId: string; chatPage?: number } | undefined
    conversationId: string | undefined
    messageIndex: number
    message: Readonly<Message> | undefined
}): MessageButtonTarget | null {
    const { character, conversationId, message } = input
    if (!character || !conversationId || input.messageIndex < 0 || !message) return null
    if (message.role !== 'user' && message.role !== 'char') return null
    return {
        characterIndex: input.characterIndex,
        chatIndex: character.chatPage ?? 0,
        messageIndex: input.messageIndex,
        messageId: message.chatId ?? null,
        role: message.role,
        characterId: character.chaId,
        conversationId,
    }
}

export function invokeMessageButton(button: MessageButtonDef, target: MessageButtonTarget): void {
    Promise.resolve(button.callback(target)).catch((error) => console.error(error))
}
