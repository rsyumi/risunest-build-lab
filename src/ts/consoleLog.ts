import { untrack } from 'svelte'

export type ConsoleLogLevel = 'log' | 'info' | 'debug' | 'warn' | 'error'
export interface ConsoleLogEntry {
    tsMs: number
    level: ConsoleLogLevel
    source: string
    message: string
}

export function isConsoleLogLevel(value: unknown): value is ConsoleLogLevel {
    return value === 'log' || value === 'info' || value === 'debug' || value === 'warn' || value === 'error'
}

export function createConsoleLogBuffer(maxEntries = 5_000, maxBytes = 8 * 1024 * 1024) {
    const entries: (ConsoleLogEntry | undefined)[] = new Array(maxEntries)
    let start = 0
    let count = 0
    let bytes = 0
    let generation = 0
    const clearListeners = new Set<(generation: number) => void>()
    const size = (entry: ConsoleLogEntry) => 2 * (entry.message.length + entry.source.length) + 64
    return {
        get generation() { return generation },
        onClear(listener: (generation: number) => void) {
            clearListeners.add(listener)
            return () => { clearListeners.delete(listener) }
        },
        append(level: ConsoleLogLevel, source: string, message: string, tsMs = Date.now()) {
            const entry = { level, source: source.slice(0, 200), message: message.slice(0, 8192), tsMs }
            const entryBytes = size(entry)
            if (entryBytes > maxBytes) return
            while (count && (count === maxEntries || bytes + entryBytes > maxBytes)) {
                bytes -= size(entries[start]!)
                entries[start] = undefined
                start = (start + 1) % maxEntries
                count--
            }
            entries[(start + count) % maxEntries] = entry
            count++
            bytes += entryBytes
        },
        snapshot(): ConsoleLogEntry[] {
            return Array.from({ length: count }, (_, index) => ({ ...entries[(start + index) % maxEntries]! }))
        },
        clear() {
            entries.fill(undefined)
            start = count = bytes = 0
            generation++
            for (const listener of clearListeners) {
                try { listener(generation) } catch { /* A closed frame must not prevent clearing. */ }
            }
        },
    }
}

export const consoleLog = createConsoleLogBuffer()

// Formatting reads logged objects, so a running effect must not subscribe to logged state.
export function installAppConsoleCapture(target: Pick<Console, ConsoleLogLevel> = console): () => void {
    return installConsoleCapture(target, (level, message) => consoleLog.append(level, 'App', message), untrack)
}

// This function is also injected into plugin frames. Keep its runtime dependencies inside it.
export function installConsoleCapture(
    target: Pick<Console, ConsoleLogLevel>,
    emit: (level: ConsoleLogLevel, message: string) => void,
    run: <T>(read: () => T) => T = (read) => read(),
): () => void {
    const levels = ['log', 'info', 'debug', 'warn', 'error'] as const
    const restorers: (() => void)[] = []
    let capturing = false
    const secretKey = /^(?:authorization|proxy-authorization|cookie|set-cookie|password|passwd|secret|token|access[_-]?token|refresh[_-]?token|api[_-]?key|x-api-key)$/i
    const mask = (text: string) => text
        .replace(/\b(Bearer|Basic)\s+[^\s,;"']+/gi, '$1 ***')
        .replace(/\b(authorization|proxy-authorization|cookie|set-cookie|password|passwd|secret|token|access[_-]?token|refresh[_-]?token|api[_-]?key|x-api-key)(["']?\s*[:=]\s*)(?:"[^"\n]*"|'[^'\n]*'|[^\s,;&}\]]+)/gi, '$1$2***')
    const format = (args: unknown[]) => {
        let remaining = 8191
        let nodes = 0
        const seen = new WeakSet<object>()
        const take = (text: string) => {
            const result = text.slice(0, remaining)
            remaining -= result.length
            return result
        }
        const visit = (value: unknown, depth: number): string => {
            if (remaining <= 0) return ''
            if (++nodes > 100) return take('[…]')
            if (typeof value === 'string') return take(mask(value.slice(0, remaining)))
            if (typeof value === 'function') return take('[Function]')
            if (!value || typeof value !== 'object') return take(String(value))
            if (seen.has(value)) return take('[Circular]')
            if (depth >= 3) return take('[Object]')
            seen.add(value)
            try {
                if (value instanceof DOMException) {
                    const name = Object.getOwnPropertyDescriptor(value, 'name')?.value
                        ?? Object.getOwnPropertyDescriptor(DOMException.prototype, 'name')?.get?.call(value)
                    const message = Object.getOwnPropertyDescriptor(value, 'message')?.value
                        ?? Object.getOwnPropertyDescriptor(DOMException.prototype, 'message')?.get?.call(value)
                    return take(String(name)) + take(': ') + visit(message, depth + 1)
                }
                if (value instanceof Error) {
                    const stack = Object.getOwnPropertyDescriptor(value, 'stack')
                    if (typeof stack?.value === 'string') return visit(stack.value, depth + 1)
                    const message = Object.getOwnPropertyDescriptor(value, 'message')
                    if (typeof message?.value === 'string') return take('Error: ') + visit(message.value, depth + 1)
                }
                const array = Array.isArray(value)
                if (array || ArrayBuffer.isView(value)) {
                    const length = array ? Object.getOwnPropertyDescriptor(value, 'length')?.value : undefined
                    let text = take('[')
                    let index = 0
                    for (; index < 30 && remaining > 0 && nodes <= 100; index++) {
                        if (typeof length === 'number' && index >= length) break
                        const descriptor = Object.getOwnPropertyDescriptor(value, String(index))
                        if (!array && !descriptor) break
                        if (index) text += take(', ')
                        text += !descriptor ? take('undefined')
                            : 'value' in descriptor ? visit(descriptor.value, depth + 1) : take('[Getter]')
                    }
                    if (typeof length === 'number' ? length > index : index === 30) text += take(', …')
                    return text + take(']')
                }
                let text = take('{')
                let keys = 0
                for (const key in value) {
                    if (remaining <= 0 || nodes > 100) break
                    if (!Object.prototype.hasOwnProperty.call(value, key)) continue
                    if (keys++ === 30) { text += take('…'); break }
                    if (keys > 1) text += take(', ')
                    text += take(key.slice(0, 100) + ': ')
                    const descriptor = Object.getOwnPropertyDescriptor(value, key)
                    text += secretKey.test(key) ? take('***')
                        : descriptor && 'value' in descriptor ? visit(descriptor.value, depth + 1)
                        : take('[Getter]')
                }
                return text + take('}')
            } catch {
                return take('[Unprintable]')
            }
        }
        let message = ''
        for (const arg of args) {
            if (remaining <= 0 || nodes > 100) break
            if (message) message += take(' ')
            message += visit(arg, 0)
        }
        return mask(message) + (remaining <= 0 || nodes > 100 ? '…' : '')
    }
    for (const level of levels) {
        const original = target[level]
        const wrapper = (...args: unknown[]) => {
            if (!capturing) {
                capturing = true
                try { emit(level, run(() => format(args))) } catch { /* Diagnostics must not break the caller. */ }
                finally { capturing = false }
            }
            return original.apply(target, args)
        }
        target[level] = wrapper
        restorers.push(() => { if (target[level] === wrapper) target[level] = original })
    }
    return () => restorers.forEach(restore => restore())
}
