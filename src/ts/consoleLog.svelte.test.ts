import { flushSync } from 'svelte'
import { describe, expect, it } from 'vitest'
import { consoleLog, installAppConsoleCapture } from './consoleLog'

describe('app console capture', () => {
    it('records logged state without subscribing the running effect to it', () => {
        const output = (..._args: unknown[]) => {}
        const target = { log: output, info: output, debug: output, warn: output, error: output }
        const dispose = installAppConsoleCapture(target)
        const state = $state({ nested: { value: 1 } })
        let runs = 0
        const stop = $effect.root(() => {
            $effect(() => {
                runs++
                target.log('state', state)
            })
        })
        try {
            flushSync()
            state.nested.value = 2
            flushSync()
            expect(runs).toBe(1)
            expect(consoleLog.snapshot().at(-1)).toMatchObject({
                level: 'log',
                source: 'App',
                message: 'state {nested: {value: 1}}',
            })
        } finally {
            stop()
            dispose()
            consoleLog.clear()
        }
    })
})
