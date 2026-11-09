import type {
    GenerationCheckpointSkipReason,
    GenerationDiagnosticOutcome,
    GenerationDiagnosticTrace,
    GenerationSavePath,
} from '../generationDiagnostics'

// Complete conversations take checkpoints because their save projection shares unchanged
// messages instead of copying and serializing them for every flush.
const COMPLETE_CONVERSATION_CHECKPOINTS = true
const CHECKPOINT_INTERVAL_MS = 2_000

interface CheckpointClock {
    setTimeout(callback: () => void, delay: number): unknown
    clearTimeout(handle: unknown): void
}

export interface GenerationCheckpointPersistence {
    /** The save path of the generation's conversation, read without capture or history reads. */
    savePath(): GenerationSavePath
    /** Whether complete conversations are saved against immutable captured values. */
    capturesImmutableCharacters(): boolean
    /** A local flush that reuses one in flight; false after a storage failure the save path reported. */
    flush(): Promise<boolean>
    clock?: CheckpointClock
}

export interface GenerationCheckpoints {
    /** Notes a streamed update applied to the owned output; the first one starts the periodic timer. */
    applied(): void
    /** Stops the timer and saves the applied output before later output stages; a fence rejects. */
    checkpoint(): Promise<void>
    stop(): void
}

const defaultClock: CheckpointClock = {
    setTimeout: (callback, delay) => setTimeout(callback, delay),
    clearTimeout: (handle) => clearTimeout(handle as ReturnType<typeof setTimeout>),
}

function ineligibility(
    persistence: GenerationCheckpointPersistence,
    savePath: GenerationSavePath,
): GenerationCheckpointSkipReason | null {
    switch (savePath) {
        case 'windowed': return null
        // Without immutable captured values a complete flush replaces the whole conversation.
        case 'complete': return !COMPLETE_CONVERSATION_CHECKPOINTS ? 'complete-gate'
            : persistence.capturesImmutableCharacters() ? null : 'missing-capture'
        case 'fallback': return 'compatibility'
        default: return 'ineligible'
    }
}

export function createGenerationCheckpoints(options: GenerationCheckpointPersistence & {
    isOwned(): boolean
    diagnostics?: GenerationDiagnosticTrace
}): GenerationCheckpoints {
    const clock = options.clock ?? defaultClock
    let timer: unknown
    let stopped = false
    let failed = false
    let dirty = false
    let periodic: Promise<void> | null = null
    let reportedSkip: GenerationCheckpointSkipReason | null = null
    const stop = () => {
        stopped = true
        if (timer !== undefined) clock.clearTimeout(timer)
        timer = undefined
    }
    const request = async (awaited: boolean): Promise<void> => {
        const savePath = options.savePath()
        const skipReason = !options.isOwned() ? 'ownership-lost'
            : failed ? 'storage-failed'
            : ineligibility(options, savePath)
        if (skipReason) {
            if (skipReason === 'ownership-lost') stop()
            // Periodic requests report a reason once rather than at every interval.
            if (awaited || reportedSkip !== skipReason) {
                options.diagnostics?.phase('checkpoint', 'exit', { outcome: 'skipped', skipReason, savePath })
            }
            if (!awaited) reportedSkip = skipReason
            return
        }
        reportedSkip = null
        if (awaited) options.diagnostics?.phase('checkpoint', 'enter', { savePath })
        let outcome: GenerationDiagnosticOutcome = 'failed'
        try {
            if (await options.flush()) {
                outcome = 'ok'
            } else {
                failed = true
                stop()
            }
        } catch (error) {
            outcome = 'stale'
            stop()
            throw error
        } finally {
            if (awaited || outcome === 'failed') options.diagnostics?.phase('checkpoint', 'exit', { outcome, savePath })
        }
    }
    const tick = () => {
        timer = undefined
        if (stopped) return
        // An unsettled request covers no later edits, so they wait for the next interval.
        if (dirty && !periodic) {
            dirty = false
            periodic = request(false).catch(stop).finally(() => { periodic = null })
        }
        if (!stopped) timer = clock.setTimeout(tick, CHECKPOINT_INTERVAL_MS)
    }
    return {
        applied() {
            if (stopped) return
            dirty = true
            if (timer === undefined) timer = clock.setTimeout(tick, CHECKPOINT_INTERVAL_MS)
        },
        async checkpoint() {
            stop()
            if (periodic) await periodic
            await request(true)
        },
        stop,
    }
}
