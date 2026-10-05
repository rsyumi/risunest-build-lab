export interface ChatScreenOwner {
    characterId: string
    conversationId: string
}

export interface ChatScreenAnchor {
    index: number
    messageId?: string
    relativeOffset: number
    latest: boolean
}

export class ChatComposerState {
    text = $state('')
    translation = $state('')
}

interface ChatScreenEntry {
    owner: ChatScreenOwner
    composer: ChatComposerState
    anchor?: ChatScreenAnchor
}

export interface SubmittedChatComposer {
    owner: ChatScreenOwner | null
    composer: ChatComposerState
    text: string
    translation: string
}

function ownerKey(owner: ChatScreenOwner): string {
    return JSON.stringify([owner.characterId, owner.conversationId])
}

export class ChatScreenStateStore {
    private entries = new Map<string, ChatScreenEntry>()

    constructor(private readonly capacity = 32) {}

    private entry(owner: ChatScreenOwner): ChatScreenEntry {
        const key = ownerKey(owner)
        const entry = this.entries.get(key) ?? {
            owner: { ...owner },
            composer: new ChatComposerState(),
        }
        this.entries.delete(key)
        this.entries.set(key, entry)
        while (this.entries.size > Math.max(1, this.capacity)) {
            this.entries.delete(this.entries.keys().next().value!)
        }
        return entry
    }

    getComposer(owner: ChatScreenOwner): ChatComposerState {
        return this.entry(owner).composer
    }

    captureComposer(owner: ChatScreenOwner): SubmittedChatComposer {
        const composer = this.getComposer(owner)
        return { owner: { ...owner }, composer, text: composer.text, translation: composer.translation }
    }

    matchesComposer(snapshot: SubmittedChatComposer): boolean {
        return (snapshot.owner === null || this.entries.get(ownerKey(snapshot.owner))?.composer === snapshot.composer) &&
            snapshot.composer.text === snapshot.text &&
            snapshot.composer.translation === snapshot.translation
    }

    clearSubmittedComposer(snapshot: SubmittedChatComposer): boolean {
        if (!this.matchesComposer(snapshot)) return false
        snapshot.composer.text = ''
        snapshot.composer.translation = ''
        return true
    }

    readAnchor(owner: ChatScreenOwner): ChatScreenAnchor | undefined {
        const anchor = this.entries.get(ownerKey(owner))?.anchor
        return anchor ? { ...anchor } : undefined
    }

    writeAnchor(owner: ChatScreenOwner, anchor: ChatScreenAnchor): void {
        this.entry(owner).anchor = { ...anchor }
    }

    remove(owner: ChatScreenOwner): void {
        this.entries.delete(ownerKey(owner))
    }

    pruneCharacters(ids: ReadonlySet<string>): void {
        for (const [key, entry] of this.entries) {
            if (!ids.has(entry.owner.characterId)) this.entries.delete(key)
        }
    }

    pruneConversations(characterId: string, ids: ReadonlySet<string>): void {
        for (const [key, entry] of this.entries) {
            if (entry.owner.characterId === characterId && !ids.has(entry.owner.conversationId)) {
                this.entries.delete(key)
            }
        }
    }

    removeCharacter(characterId: string): void {
        this.pruneConversations(characterId, new Set())
    }

    clear(): void {
        this.entries.clear()
    }
}

export const chatScreenState = new ChatScreenStateStore()
