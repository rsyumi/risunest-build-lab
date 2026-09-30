import { describe, expect, it } from 'vitest'
import type { character } from './database.svelte'
import { WorkingSetResidencyRegistry } from './workingSetResidency'
import { clearStreamingConversation, restoreStreamingConversationState, setStreamingConversation } from './streamingConversationRegistry'

describe('runtime streaming ownership', () => {
    it('guards release after persisted-shape flags are normalized and restores display mode on refresh', () => {
        const character = { chaId: 'character', type: 'character', chats: [{ id: 'chat', message: [], isStreaming: false }] } as unknown as character
        const registry = new WorkingSetResidencyRegistry()
        setStreamingConversation('chat', 'balanced')
        try {
            expect(registry.canReleaseConversation(character, 'chat')).toBe(false)
            expect(registry.canReleaseCharacterToCatalog({ characters: [character] } as any, 'character')).toBe(false)
            restoreStreamingConversationState(character.chats[0])
            expect(character.chats[0]).toMatchObject({ isStreaming: true, activeStreamingDisplayOptimizationMode: 'balanced' })
        } finally { clearStreamingConversation('chat') }
        character.chats[0].isStreaming = false
        expect(registry.canReleaseConversation(character, 'chat')).toBe(true)
    })
})
