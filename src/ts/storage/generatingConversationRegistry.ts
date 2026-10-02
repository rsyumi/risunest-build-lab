import type { GeneratingConversation } from './persistentDataStore'

export function createGeneratingConversationRegistry() {
    const entries = new Map<string, { target: GeneratingConversation; owners: Set<symbol> }>()
    const key = (target: GeneratingConversation) => JSON.stringify([target.characterId, target.conversationId])
    return {
        register(target: GeneratingConversation): () => void {
            if (!target.characterId || !target.conversationId) throw new TypeError('Generation requires stable IDs')
            const identity = key(target), owner = Symbol()
            const entry = entries.get(identity) ?? { target: { ...target }, owners: new Set<symbol>() }
            entry.owners.add(owner)
            entries.set(identity, entry)
            return () => {
                entry.owners.delete(owner)
                if (!entry.owners.size && entries.get(identity) === entry) entries.delete(identity)
            }
        },
        complete(target: GeneratingConversation): void { entries.delete(key(target)) },
        snapshot(): GeneratingConversation[] { return [...entries.values()].map((entry) => ({ ...entry.target })) },
    }
}

export const generatingConversations = createGeneratingConversationRegistry()
export const registerGeneratingConversation = generatingConversations.register
