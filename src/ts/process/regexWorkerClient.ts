import type { DisplayCbsContext, DisplayCbsPlan } from './displayCbsCompiler'
import type { RegexExecutionPlan, RegexExecutionResult } from './regexExecutionPlan'
import type { RegexOperation, RegexOperationResults } from './regexOperations'

export const DEFAULT_REGEX_WORKER_TIMEOUT_MS = 10_000
export const REGEX_WORKER_READY_TIMEOUT_MS = 10_000

export type RegexWorkerPlanEntry = [
    sourceIndex: number,
    pattern: string,
    replacement: string,
    flags: string,
    host?: boolean,
    conditional?: boolean,
]

export type RegexWorkerRequest = {
    type: 'register'
    revision: number
    entries: RegexWorkerPlanEntry[]
    displayCbs?: DisplayCbsPlan
} | {
    type: 'execute'
    id: number
    revision: number
    input: string
    continuation?: { start: number, end: number, skipPlainCbs?: boolean }
    reuseInput?: boolean
    cbsContext?: DisplayCbsContext
} | {
    type: 'operation'
    id: number
    operation: RegexOperation
} | {
    type: 'release'
}

export type RegexWorkerResponse = { type: 'ready' } | {
    type: 'result'
    id: number
    data: string
    errors: [sourceIndex: number, message: string][]
    steps?: { index: number, matched: boolean }[]
    nextIndex?: number
    cbsReads?: string[]
    checkpointInput?: string
    unchangedCbsFallback?: boolean
} | {
    type: 'operationResult'
    id: number
    value: RegexOperationResults[keyof RegexOperationResults]
} | {
    type: 'error'
    id: number
    message: string
}

export interface RegexWorkerLike {
    postMessage(message: RegexWorkerRequest): void
    terminate(): void
    addEventListener(type: 'message' | 'error', listener: EventListener): void
    removeEventListener(type: 'message' | 'error', listener: EventListener): void
}

export interface RegexWorkerExecuteOptions {
    signal?: AbortSignal
    timeoutMs?: number
    continuation?: { start: number, end: number, skipPlainCbs?: boolean }
    cbsContext?: DisplayCbsContext
}

interface PendingRequest {
    plan?: RegexExecutionPlan
    operation?: RegexOperation
    cbsContext?: DisplayCbsContext
    continuation?: { start: number, end: number, skipPlainCbs?: boolean }
    input: string
    timeoutMs: number
    signal?: AbortSignal
    abortListener?: () => void
    timeout?: ReturnType<typeof setTimeout>
    resolve: (result: any) => void
    reject: (error: unknown) => void
}

export class RegexExecutionTimeoutError extends Error {
    readonly category = 'regex_timeout'

    constructor(readonly revision: number) {
        super('Regex script execution timed out.')
        this.name = 'RegexExecutionTimeoutError'
    }
}

function createAbortError(signal: AbortSignal): unknown {
    if (signal.reason !== undefined) {
        return signal.reason
    }
    return new DOMException('The operation was aborted', 'AbortError')
}

function createModuleWorker(): RegexWorkerLike {
    return new Worker(new URL('./regexWorker.ts', import.meta.url), { type: 'module' }) as unknown as RegexWorkerLike
}

export class RegexWorkerResetError extends Error {
    constructor() {
        super('Regex worker was reset by another request')
        this.name = 'RegexWorkerResetError'
    }
}

export class RegexWorkerUnavailableError extends Error {
    constructor() {
        super('Regex worker did not become ready')
        this.name = 'RegexWorkerUnavailableError'
    }
}

export class RegexWorkerClient {
    private worker: RegexWorkerLike | undefined
    private ready = false
    private bootTimeout?: ReturnType<typeof setTimeout>
    private registeredRevision: number | undefined
    private continuation?: { revision: number, data: string }
    private readonly pending = new Map<number, PendingRequest>()
    private activeId: number | undefined
    private nextRequestId = 1
    private workerListeners?: { worker: RegexWorkerLike; messageListener: EventListener; errorListener: EventListener }

    constructor(private readonly workerFactory: () => RegexWorkerLike = createModuleWorker) {}

    execute(plan: RegexExecutionPlan, input: string, options: RegexWorkerExecuteOptions = {}): Promise<RegexExecutionResult> {
        return this.enqueue({ plan, input, continuation: options.continuation, cbsContext: options.cbsContext }, options)
    }

    operate<K extends keyof RegexOperationResults>(operation: RegexOperation<K>, options: RegexWorkerExecuteOptions = {}): Promise<RegexOperationResults[K]> {
        return this.enqueue({ operation, input: operation.input }, options)
    }

    releaseContinuation(): void {
        this.continuation = undefined
        if (this.ready && this.activeId === undefined && this.pending.size === 0) {
            try { this.worker?.postMessage({ type: 'release' }) }
            catch (error) { this.replaceWorker(error) }
        }
    }

    private enqueue<T>(work: Pick<PendingRequest, 'plan' | 'operation' | 'input' | 'continuation' | 'cbsContext'>, options: RegexWorkerExecuteOptions): Promise<T> {
        if (options.signal?.aborted) return Promise.reject(createAbortError(options.signal))
        return new Promise((resolve, reject) => {
            const id = this.nextRequestId++
            const request: PendingRequest = {
                ...work, signal: options.signal,
                timeoutMs: options.timeoutMs ?? DEFAULT_REGEX_WORKER_TIMEOUT_MS,
                resolve, reject,
            }
            if (options.signal) {
                request.abortListener = () => {
                    if (this.activeId === id) this.replaceWorker(createAbortError(options.signal!), id)
                    else {
                        this.pending.delete(id)
                        this.cleanupPending(request)
                        reject(createAbortError(options.signal!))
                    }
                }
                options.signal.addEventListener('abort', request.abortListener, { once: true })
            }
            this.pending.set(id, request)
            try {
                this.ensureWorker()
                this.dispatchNext()
            } catch (error) {
                this.replaceWorker(error, id)
            }
        })
    }

    private ensureWorker(): void {
        if (this.worker) return
        const worker = this.workerFactory()
        this.worker = worker
        const messageListener: EventListener = (event) => {
            if (this.worker !== worker) return
            const response = (event as MessageEvent<RegexWorkerResponse>).data
            if (response.type === 'ready') {
                clearTimeout(this.bootTimeout)
                this.ready = true
                this.dispatchNext()
                return
            }
            const request = this.pending.get(response.id)
            if (!request || this.activeId !== response.id) return
            this.pending.delete(response.id)
            this.activeId = undefined
            this.cleanupPending(request)
            if (response.type === 'error') {
                const error = new Error(response.message)
                error.name = 'RegexWorkerOperationError'
                request.reject(error)
            }
            else if (response.type === 'operationResult') request.resolve(response.value)
            else {
                this.continuation = request.continuation && request.plan
                    ? { revision: request.plan.revision, data: response.data } : undefined
                request.resolve({
                    data: response.data,
                    errors: response.errors.map(([sourceIndex, message]) => ({ sourceIndex, error: new Error(message) })),
                    ...(response.cbsReads ? { cbsReads: response.cbsReads } : {}),
                    ...(response.checkpointInput !== undefined ? { checkpointInput: response.checkpointInput } : {}),
                    ...(response.unchangedCbsFallback ? { unchangedCbsFallback: true } : {}),
                    ...(response.steps ? { steps: response.steps, nextIndex: response.nextIndex } : {}),
                })
            }
            this.dispatchNext()
        }
        const errorListener: EventListener = (event) => {
            if (this.worker !== worker) return
            const error = event as ErrorEvent
            this.replaceWorker(error.error ?? new Error(error.message || 'Regex worker failed'), this.activeId)
        }
        this.workerListeners = { worker, messageListener, errorListener }
        this.bootTimeout = setTimeout(() => this.replaceWorker(new RegexWorkerUnavailableError()), REGEX_WORKER_READY_TIMEOUT_MS)
        worker.addEventListener('message', messageListener)
        worker.addEventListener('error', errorListener)
    }

    private dispatchNext(): void {
        if (!this.worker || !this.ready || this.activeId !== undefined) return
        const next = this.pending.entries().next().value as [number, PendingRequest] | undefined
        if (!next) return
        const [id, request] = next
        this.activeId = id
        request.timeout = setTimeout(() => this.replaceWorker(new RegexExecutionTimeoutError(request.plan?.revision ?? 0), id), request.timeoutMs)
        try {
            if (request.operation) {
                this.worker.postMessage({ type: 'operation', id, operation: request.operation })
                return
            }
            const plan = request.plan!
            if (this.registeredRevision !== plan.revision) {
                this.continuation = undefined
                this.registeredRevision = plan.revision
                this.worker.postMessage({ type: 'register', revision: plan.revision,
                    ...(plan.displayCbs ? { displayCbs: plan.displayCbs } : {}),
                    entries: plan.entries.map((entry) => [entry.sourceIndex, entry.pattern, entry.replacement, entry.flags,
                        entry.requiresHostExecution || entry.dynamicPattern, entry.actions.length > 0]),
                })
            }
            const reuseInput = request.continuation !== undefined
                && this.continuation?.revision === plan.revision && this.continuation.data === request.input
            this.worker.postMessage({ type: 'execute', id, revision: plan.revision, input: reuseInput ? '' : request.input,
                ...(reuseInput ? { reuseInput: true } : {}),
                ...(request.cbsContext ? { cbsContext: request.cbsContext } : {}),
                ...(request.continuation ? { continuation: request.continuation } : {}),
            })
        } catch (error) {
            this.replaceWorker(error, id)
        }
    }

    private replaceWorker(error: unknown, culpritId?: number): void {
        const listeners = this.workerListeners
        this.worker = undefined
        this.workerListeners = undefined
        this.registeredRevision = undefined
        this.continuation = undefined
        this.ready = false
        this.activeId = undefined
        clearTimeout(this.bootTimeout)
        if (listeners) {
            listeners.worker.removeEventListener('message', listeners.messageListener)
            listeners.worker.removeEventListener('error', listeners.errorListener)
            listeners.worker.terminate()
        }
        const requests = [...this.pending.entries()]
        this.pending.clear()
        for (const [id, request] of requests) {
            this.cleanupPending(request)
            request.reject(culpritId === undefined || id === culpritId ? error : new RegexWorkerResetError())
        }
    }

    private cleanupPending(request: PendingRequest): void {
        clearTimeout(request.timeout)
        if (request.signal && request.abortListener) request.signal.removeEventListener('abort', request.abortListener)
    }
}

let sharedRegexWorkerClient: RegexWorkerClient | undefined

export function getSharedRegexWorkerClient(): RegexWorkerClient {
    sharedRegexWorkerClient ??= new RegexWorkerClient()
    return sharedRegexWorkerClient
}

export function isRegexWorkerAvailable(): boolean {
    return typeof Worker !== 'undefined'
}
