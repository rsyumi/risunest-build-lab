import type { RegexWorkerLike, RegexWorkerRequest, RegexWorkerResponse } from '../../src/ts/process/regexWorkerClient'
import { createRegexWorkerMessageHandler } from '../../src/ts/process/regexWorker'

export class FakeWorker implements RegexWorkerLike {
    constructor(private readonly autoReady = true) {}
    readonly requests: RegexWorkerRequest[] = []
    terminated = false
    private readonly messageListeners = new Set<(event: MessageEvent<RegexWorkerResponse>) => void>()
    private readonly errorListeners = new Set<(event: ErrorEvent) => void>()

    postMessage(message: RegexWorkerRequest): void {
        this.requests.push(message)
    }

    terminate(): void {
        this.terminated = true
    }

    addEventListener(type: 'message' | 'error', listener: EventListener): void {
        if (type === 'message') {
            this.messageListeners.add(listener as (event: MessageEvent<RegexWorkerResponse>) => void)
            if (this.autoReady) this.respond({ type: 'ready' })
        }
        else {
            this.errorListeners.add(listener as (event: ErrorEvent) => void)
        }
    }

    removeEventListener(type: 'message' | 'error', listener: EventListener): void {
        if (type === 'message') {
            this.messageListeners.delete(listener as (event: MessageEvent<RegexWorkerResponse>) => void)
        }
        else {
            this.errorListeners.delete(listener as (event: ErrorEvent) => void)
        }
    }

    respond(response: RegexWorkerResponse): void {
        for (const listener of this.messageListeners) {
            listener({ data: response } as MessageEvent<RegexWorkerResponse>)
        }
    }

    get listenerCount(): number {
        return this.messageListeners.size + this.errorListeners.size
    }
}

export class InMemoryWorker extends FakeWorker {
    private readonly handleRequest = createRegexWorkerMessageHandler((response) => {
        this.respond(response)
    })

    override postMessage(message: RegexWorkerRequest): void {
        super.postMessage(message)
        this.handleRequest(message)
    }
}

export class StallingWorker extends InMemoryWorker {
    private entries: Extract<RegexWorkerRequest, { type: 'register' }>['entries'] = []
    override postMessage(message: RegexWorkerRequest): void {
        if (message.type === 'register') this.entries = message.entries
        if (message.type === 'operation' && message.operation.pattern.startsWith('SLOW')) return
        if (message.type === 'execute') {
            const { start = 0, end = this.entries.length } = message.continuation ?? {}
            if (this.entries.slice(start, end).some((entry) => entry[1].startsWith('SLOW'))) return
        }
        super.postMessage(message)
    }
}
