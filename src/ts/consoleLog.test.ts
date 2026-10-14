import { describe, expect, it } from 'vitest'
import { createConsoleLogBuffer, installConsoleCapture, type ConsoleLogLevel } from './consoleLog'

describe('console diagnostics', () => {
    it('keeps the newest entries within both count and byte limits and clears them', () => {
        const buffer = createConsoleLogBuffer(3, 250)
        for (let n = 0; n < 10; n++) buffer.append('log', 'App', String(n), n)
        expect(buffer.snapshot().map(entry => entry.message)).toEqual(['7', '8', '9'])
        buffer.append('error', 'plugin', 'x'.repeat(60), 10)
        expect(buffer.snapshot().map(entry => entry.message)).toEqual(['x'.repeat(60)])
        const snapshot = buffer.snapshot()
        snapshot[0].message = 'changed'
        expect(buffer.snapshot()[0].message).toBe('x'.repeat(60))
        buffer.clear()
        expect(buffer.snapshot()).toEqual([])
        buffer.append('info', 'App', 'after clear')
        expect(buffer.snapshot()[0].message).toBe('after clear')
    })

    it('captures every level without replacing original output or retaining mutable objects', () => {
        const originals: unknown[][] = []
        const output = (...args: unknown[]) => { originals.push(args) }
        const target = { log: output, info: output, debug: output, warn: output, error: output }
        const captured: { level: ConsoleLogLevel; message: string }[] = []
        const dispose = installConsoleCapture(target, (level, message) => captured.push({ level, message }))
        const object = { count: 1 }
        for (const level of ['log', 'info', 'debug', 'warn', 'error'] as const) target[level](level, object)
        object.count = 2
        expect(captured).toEqual(['log', 'info', 'debug', 'warn', 'error'].map(level => ({ level, message: `${level} {count: 1}` })))
        expect(originals).toEqual(['log', 'info', 'debug', 'warn', 'error'].map(level => [level, object]))
        dispose()
        target.log('not captured')
        expect(captured).toHaveLength(5)
        expect(originals.at(-1)).toEqual(['not captured'])
    })

    it('bounds formatting and handles secrets, cycles and getters without executing them', () => {
        const messages: string[] = []
        const noop = () => {}
        const target = { log: noop, info: noop, debug: noop, warn: noop, error: noop } as Pick<Console, ConsoleLogLevel>
        let reads = 0
        const object: Record<string, unknown> = { apiKey: 'synthetic-secret', nested: { token: 'synthetic-token' } }
        object.self = object
        Object.defineProperty(object, 'getter', { enumerable: true, get() { reads++; throw new Error('getter') } })
        const dispose = installConsoleCapture(target, (_, message) => messages.push(message))
        target.log(object)
        target.log('Authorization: Bearer synthetic-bearer password=synthetic-password')
        target.log(new Proxy({}, { ownKeys() { throw new Error('proxy') } }))
        target.log('a'.repeat(8193))
        target.error(new Error('synthetic stack'))
        expect(reads).toBe(0)
        expect(messages[0]).toContain('[Circular]')
        expect(messages[0]).toContain('[Getter]')
        expect(messages.join()).not.toMatch(/synthetic-(secret|token|bearer|password)/)
        expect(messages[2]).toContain('[Unprintable]')
        expect(messages[3]).toHaveLength(8192)
        expect(messages[4]).toContain('Error: synthetic stack')
        const error = new Error('synthetic stack')
        Object.defineProperty(error, 'stack', { value: 'Error: synthetic stack\n    at synthetic-plugin.js:10' })
        target.error(error)
        expect(messages[5]).toContain('at synthetic-plugin.js:10')
        target.log('token=', 'synthetic-split-secret')
        expect(messages[6]).not.toContain('synthetic-split-secret')
        target.log(new Uint8Array(31), Array(31).fill(7))
        expect(messages[7]).toBe(`[${Array(30).fill(0).join(', ')}, …] [${Array(30).fill(7).join(', ')}, …]`)
        target.error(new DOMException('synthetic access denied', 'NotReadableError'))
        expect(messages[8]).toBe('NotReadableError: synthetic access denied')
        dispose()
    })

    it('contains sink failures and recursive logging without interrupting original calls', () => {
        const output: unknown[][] = []
        const original = (...args: unknown[]) => { output.push(args) }
        const target = { log: original, info: original, debug: original, warn: original, error: original }
        const dispose = installConsoleCapture(target, () => {
            target.warn('nested')
            throw new Error('sink failed')
        })
        expect(() => target.log('outer')).not.toThrow()
        expect(output).toEqual([['nested'], ['outer']])
        dispose()
    })
})
