export type GenerationEndStatus = 'completed' | 'failed' | 'aborted'

export interface GenerationEndTarget {
    characterId: string
    conversationId: string
}

export interface GenerationEndRecord extends GenerationEndTarget {
    status: GenerationEndStatus
    reroll: boolean
    /** IDs of the messages the generation wrote to, in the order it noted them. */
    messageIds: string[]
}

export interface GenerationEndRun {
    /** Runs one generation step; only steps run through here are attributed to this run. */
    capture<T>(generate: () => Promise<T>): Promise<T>
    /** Reports the run once, if any captured step entered generation. */
    finish(status: GenerationEndStatus): void
}

interface ActiveRun extends GenerationEndTarget {
    started: boolean
    messageIds: string[]
}

let active: ActiveRun | null = null
const listeners = new Set<(record: GenerationEndRecord) => void>()

function capturing(target: GenerationEndTarget | undefined): ActiveRun | null {
    if (!active || !target) return null
    return active.characterId === target.characterId && active.conversationId === target.conversationId
        ? active
        : null
}

/** Collects what a chat-screen or plugin `sendChat` generation on `target` does, for the generation end event. */
export function beginGenerationEndRun(
    target: GenerationEndTarget,
    options: { reroll?: boolean } = {},
): GenerationEndRun {
    const run: ActiveRun = {
        characterId: target.characterId,
        conversationId: target.conversationId,
        started: false,
        messageIds: [],
    }
    let finished = false
    return {
        async capture(generate) {
            active = run
            try {
                return await generate()
            } finally {
                if (active === run) active = null
            }
        },
        finish(status) {
            if (finished) return
            finished = true
            if (active === run) active = null
            if (!run.started) return
            const record: GenerationEndRecord = {
                characterId: run.characterId,
                conversationId: run.conversationId,
                status,
                reroll: options.reroll === true,
                messageIds: [...run.messageIds],
            }
            for (const listener of [...listeners]) {
                try {
                    listener(record)
                } catch (error) {
                    console.error(error)
                }
            }
        },
    }
}

export function noteGenerationStarted(target: GenerationEndTarget | undefined): void {
    const run = capturing(target)
    if (run) run.started = true
}

export function noteGenerationMessage(target: GenerationEndTarget | undefined, messageId: string | undefined): void {
    const run = capturing(target)
    if (run && messageId && !run.messageIds.includes(messageId)) run.messageIds.push(messageId)
}

export function subscribeGenerationEnd(listener: (record: GenerationEndRecord) => void): () => void {
    listeners.add(listener)
    return () => {
        listeners.delete(listener)
    }
}
