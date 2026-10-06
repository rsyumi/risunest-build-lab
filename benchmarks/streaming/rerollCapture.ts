import { generateResponseCandidate } from '../../src/ts/durableReroll'

type Options = Parameters<typeof generateResponseCandidate>[0]
type Phase = 'admission' | 'checkpoint' | 'generation' | 'generation-completed' | 'provider-completed' | 'settled' | 'failed' | 'observation'
type ErrorCategory = 'abort' | 'type' | 'range' | 'error' | 'non-error' | null

export interface RerollSnapshot {
    sequence: number
    phase: Phase
    ownerId: string | null
    targetId: string | null
    controllerValid: boolean | null
    pendingSave: boolean
    providerCompleted: boolean
    generationCompleted: boolean
    result: boolean | null
    messageCount: number | null
    recoveryPhase: 'prepared' | 'generating' | null
    attemptId: string | null
    recoveryStart: number | null
    errorCategory: ErrorCategory
    rendererExit: 'not-observed' | 'exited' | 'crashed'
}

function identifier(value: unknown): string | null {
    return typeof value === 'string' && /^[a-zA-Z0-9:_-]{1,96}$/.test(value) ? value : null
}

function category(error: unknown): ErrorCategory {
    if (error instanceof DOMException && error.name === 'AbortError') return 'abort'
    if (error instanceof TypeError) return 'type'
    if (error instanceof RangeError) return 'range'
    return error instanceof Error ? 'error' : 'non-error'
}

// Only synthetic harnesses may supply options or consume these snapshots.
export function captureSyntheticReroll(
    options: Options,
    ownerId: string,
    emit: (snapshot: RerollSnapshot) => void = () => {},
) {
    let sequence = 0
    let pendingSave = false
    let providerCompleted = false
    let generationCompleted = false
    let result: boolean | null = null
    let errorCategory: ErrorCategory = null
    let rendererExit: RerollSnapshot['rendererExit'] = 'not-observed'
    let chat = options.chat
    const snapshots: RerollSnapshot[] = []
    const record = (phase: Phase, readState = rendererExit === 'not-observed') => {
        let controllerValid: boolean | null = null
        if (readState) {
            try { controllerValid = options.isCurrent() } catch { /* Preserve unavailable state. */ }
        }
        const snapshot: RerollSnapshot = {
            sequence: ++sequence,
            phase,
            ownerId: identifier(ownerId),
            targetId: identifier(chat.id),
            controllerValid,
            pendingSave,
            providerCompleted,
            generationCompleted,
            result,
            messageCount: readState ? chat.message.length : null,
            recoveryPhase: readState ? chat.rerollRecovery?.phase ?? null : null,
            attemptId: readState ? identifier(chat.rerollRecovery?.attemptId) : null,
            recoveryStart: readState ? chat.rerollRecovery?.startIndex ?? null : null,
            errorCategory,
            rendererExit,
        }
        snapshots.push(snapshot)
        if (snapshots.length > 64) snapshots.shift()
        emit({ ...snapshot })
        return { ...snapshot }
    }
    return {
        snapshots: () => snapshots.map(snapshot => ({ ...snapshot })),
        observe: () => record('observation'),
        // Call from the synthetic provider stub, independently of generate() settlement.
        providerCompleted: () => {
            providerCompleted = true
            return record('provider-completed')
        },
        rendererExited: (crashed: boolean) => {
            rendererExit = crashed ? 'crashed' : 'exited'
            return record('observation', false)
        },
        async run() {
            record('admission')
            try {
                result = await generateResponseCandidate({
                    ...options,
                    currentChat: async () => {
                        chat = await options.currentChat?.() ?? chat
                        return chat
                    },
                    flush: async () => {
                        pendingSave = true
                        record('checkpoint')
                        try { await options.flush() }
                        finally { pendingSave = false; record('checkpoint') }
                    },
                    generate: async () => {
                        record('generation')
                        const generated = await options.generate()
                        generationCompleted = true
                        record('generation-completed')
                        return generated
                    },
                })
                record('settled')
                return result
            } catch (error) {
                errorCategory = category(error)
                record('failed')
                throw error
            }
        },
    }
}
