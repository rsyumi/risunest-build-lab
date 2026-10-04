import type { GeneratingConversation } from '../storage/persistentDataStore'

// A generation's request is pending between prompt building and the output message.
// Plugin patches of plugin-owned fields may commit only then, and the generation waits
// for the patches in flight before it applies the response.

const key = (target: GeneratingConversation) => JSON.stringify([target.characterId, target.conversationId])
const openPhases = new Map<string, number>()
const patchesInFlight = new Map<string, Set<Promise<void>>>()

export function isGenerationRequestPhaseOpen(target: GeneratingConversation): boolean {
    return openPhases.has(key(target))
}

/** Opens the request phase; the returned close stops admitting patches and waits for those in flight. */
export function openGenerationRequestPhase(target: GeneratingConversation): () => Promise<void> {
    const identity = key(target)
    openPhases.set(identity, (openPhases.get(identity) ?? 0) + 1)
    let closed = false
    return async () => {
        if (!closed) {
            closed = true
            const count = openPhases.get(identity)! - 1
            if (count > 0) openPhases.set(identity, count)
            else openPhases.delete(identity)
        }
        await settleConversationPatches(target)
    }
}

export function trackConversationPatch(target: GeneratingConversation): () => void {
    const identity = key(target)
    let finish!: () => void
    const settled = new Promise<void>((resolve) => { finish = resolve })
    const patches = patchesInFlight.get(identity) ?? new Set<Promise<void>>()
    patches.add(settled)
    patchesInFlight.set(identity, patches)
    return () => {
        finish()
        patches.delete(settled)
        if (!patches.size && patchesInFlight.get(identity) === patches) patchesInFlight.delete(identity)
    }
}

export async function settleConversationPatches(target: GeneratingConversation): Promise<void> {
    const identity = key(target)
    for (let patches = patchesInFlight.get(identity); patches?.size; patches = patchesInFlight.get(identity)) {
        await Promise.all([...patches])
    }
}
