import type { RegexExecutionPlan, RegexExecutionResult } from './regexExecutionPlan'

export const DEFAULT_REGEX_WORKER_TIMEOUT_MS = 2_000
export const REGEX_WORKER_READY_TIMEOUT_MS = 10_000

export type RegexWorkerPlanEntry = [
    sourceIndex: number,
    pattern: string,
    replacement: string,
    flags: string,
]

export type RegexWorkerRequest = {
    type: 'register'
    revision: number
    entries: RegexWorkerPlanEntry[]
} | {
    type: 'execute'
    id: number
    revision: number
    input: string
}

export type RegexWorkerResponse = { type: 'ready' } | {
    type: 'result'
    id: number
    data: string
    errors: [sourceIndex: number, message: string][]
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
}

interface PendingRequest {
    plan: RegexExecutionPlan
    input: string
    timeoutMs: number
    signal?: AbortSignal
    abortListener?: () => void
    timeout?: ReturnType<typeof setTimeout>
    resolve: (result: RegexExecutionResult) => void
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
    private readonly pending = new Map<number, PendingRequest>()
    private activeId: number | undefined
    private nextRequestId = 1
    private workerListeners?: { worker: RegexWorkerLike; messageListener: EventListener; errorListener: EventListener }

    constructor(private readonly workerFactory: () => RegexWorkerLike = createModuleWorker) {}

    execute(plan: RegexExecutionPlan, input: string, options: RegexWorkerExecuteOptions = {}): Promise<RegexExecutionResult> {
        if (options.signal?.aborted) return Promise.reject(createAbortError(options.signal))
        return new Promise((resolve, reject) => {
            const id = this.nextRequestId++
            const request: PendingRequest = {
                plan, input, signal: options.signal,
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
            if (response.type === 'error') request.reject(new Error(response.message))
            else request.resolve({
                data: response.data,
                errors: response.errors.map(([sourceIndex, message]) => ({ sourceIndex, error: new Error(message) })),
            })
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
        request.timeout = setTimeout(() => this.replaceWorker(new RegexExecutionTimeoutError(request.plan.revision), id), request.timeoutMs)
        try {
            if (this.registeredRevision !== request.plan.revision) {
                this.registeredRevision = request.plan.revision
                this.worker.postMessage({ type: 'register', revision: request.plan.revision,
                    entries: request.plan.entries.map((entry) => [entry.sourceIndex, entry.pattern, entry.replacement, entry.flags]),
                })
            }
            this.worker.postMessage({ type: 'execute', id, revision: request.plan.revision, input: request.input })
        } catch (error) {
            this.replaceWorker(error, id)
        }
    }

    private replaceWorker(error: unknown, culpritId?: number): void {
        const listeners = this.workerListeners
        this.worker = undefined
        this.workerListeners = undefined
        this.registeredRevision = undefined
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
