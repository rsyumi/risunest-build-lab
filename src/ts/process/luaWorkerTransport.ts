import { luaErrorMessage, type LuaWorkerCommand, type LuaWorkerEvent, type LuaWorkerRequest, type LuaWorkerResponse } from './luaWorkerProtocol'

type Listener = { receive(event: LuaWorkerEvent): void; fail(error: Error): void }
const createDefaultWorker = () => new Worker(new URL('./luaWorker.ts', import.meta.url), { type: 'module' })
const transports = new WeakMap<() => Worker, LuaWorkerTransport>()

export class LuaWorkerInterruptedError extends Error {}

/** One JS isolate; each engine keeps its own WASM memory and Lua globals. */
export class LuaWorkerTransport {
    private readonly listeners = new Map<number, Listener>()
    private readonly active = new Map<number, number>()
    private readonly worker: Worker
    private sequence = 0
    private heartbeat = 0
    private watchdog?: ReturnType<typeof setInterval>
    private closed = false
    private running?: { engine: number; started: number }
    private executionMs = 0
    private hasExecuted = false
    private probe?: { token: number; started: number }
    private probeSequence = 0

    static connect(listener: Listener, createWorker = createDefaultWorker) {
        let transport = transports.get(createWorker)
        if (!transport) {
            transport = new LuaWorkerTransport(createWorker)
            transports.set(createWorker, transport)
        }
        const engine = ++transport.sequence
        transport.listeners.set(engine, listener)
        return { transport, engine }
    }

    private constructor(private readonly createWorker: () => Worker) {
        this.worker = createWorker()
        this.worker.onmessage = (message: MessageEvent<LuaWorkerResponse>) => {
            this.heartbeat = performance.now()
            if (message.data.event.kind === 'pong') {
                if (this.probe?.token === message.data.event.token) this.probe = undefined
                return
            }
            if (message.data.event.kind === 'execution') {
                if (message.data.event.running) {
                    this.hasExecuted = true
                    this.running = { engine: message.data.engine, started: performance.now() }
                }
                else if (this.running?.engine === message.data.engine) {
                    this.executionMs += performance.now() - this.running.started
                    this.running = undefined
                }
                return
            }
            if (message.data.event.kind === 'disposed') { this.end(message.data.engine); return }
            this.listeners.get(message.data.engine)?.receive(message.data.event)
        }
        this.worker.onerror = event => {
            event.preventDefault()
            this.fail(new Error(event.message || 'Lua worker failed'))
        }
        this.worker.onmessageerror = () => this.fail(new Error('Lua worker message could not be read'))
    }

    begin(engine: number, timeout: number): void {
        this.active.set(engine, timeout)
        if (this.watchdog !== undefined) return
        this.heartbeat = performance.now()
        let checkedAt = this.heartbeat
        this.watchdog = setInterval(() => {
            const now = performance.now()
            const delayed = now - checkedAt > 1_000
            checkedAt = now
            const running = this.running
            const deadline = Math.max(5_000, running ? this.active.get(running.engine) ?? 5_000 : 5_000) + 1_000
            if (now - (running?.started ?? this.heartbeat) > deadline) {
                // A blocked UI may process this timer before queued Worker completion.
                // Ask for a fresh response before destroying every retained engine.
                if (!this.probe || delayed) {
                    this.probe = { token: ++this.probeSequence, started: now }
                    try { this.worker.postMessage({ engine: 0, command: { kind: 'ping', token: this.probe.token } } satisfies LuaWorkerRequest) }
                    catch (error) { this.fail(new Error(luaErrorMessage(error))) }
                    return
                }
                if (now - this.probe.started <= 1_000) return
                const listeners = [...this.listeners]
                this.shutdown()
                for (const [engine, listener] of listeners) {
                    const message = !running ? 'Lua worker did not respond'
                        : engine === running.engine ? 'thread timeout exceeded' : 'Lua worker stopped while another engine was running'
                    listener.fail(this.hasExecuted && engine !== running?.engine
                        ? new LuaWorkerInterruptedError(message) : new Error(message))
                }
            } else this.probe = undefined
        }, 250)
    }

    startupClock(): number {
        const now = performance.now()
        return now - this.executionMs - (this.running ? now - this.running.started : 0)
    }

    end(engine: number): void {
        this.active.delete(engine)
        if (!this.active.size) {
            clearInterval(this.watchdog)
            this.watchdog = undefined
            this.probe = undefined
        }
    }

    send(engine: number, command: LuaWorkerCommand | { kind: 'dispose' }): void {
        if (this.closed) return
        try { this.worker.postMessage({ engine, command } satisfies LuaWorkerRequest) }
        catch (error) { this.listeners.get(engine)?.fail(new Error(luaErrorMessage(error))) }
    }

    release(engine: number): void {
        this.listeners.delete(engine)
        if (this.listeners.size) this.send(engine, { kind: 'dispose' })
        else this.shutdown()
    }

    fail(error: Error): void {
        const listeners = [...this.listeners.values()]
        this.shutdown()
        for (const listener of listeners) listener.fail(this.hasExecuted
            ? new LuaWorkerInterruptedError(error.message, { cause: error }) : error)
    }

    private shutdown(): void {
        if (this.closed) return
        this.closed = true
        transports.delete(this.createWorker)
        clearInterval(this.watchdog)
        this.active.clear()
        this.listeners.clear()
        this.worker.terminate()
    }
}
