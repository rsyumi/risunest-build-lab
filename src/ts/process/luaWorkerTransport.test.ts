// @vitest-environment node

import { expect, test, vi } from 'vitest'
import { LuaWorkerTransport } from './luaWorkerTransport'
import type { LuaWorkerEvent, LuaWorkerResponse } from './luaWorkerProtocol'

test.each([false, true])('accepts queued completion after the main thread stalls (also during probe: %s)', stallDuringProbe => {
    vi.useFakeTimers({ toFake: ['setInterval', 'clearInterval'] })
    let now = 0
    const clock = vi.spyOn(performance, 'now').mockImplementation(() => now)
    const received: LuaWorkerEvent[] = []
    const failures: Error[] = []
    const worker = {
        onmessage: undefined as ((event: { data: LuaWorkerResponse }) => void) | undefined,
        postMessage() {}, terminate() {},
    }
    const { transport, engine } = LuaWorkerTransport.connect({
        receive: event => received.push(event), fail: error => failures.push(error),
    }, () => worker as unknown as Worker)
    const deliver = (event: LuaWorkerEvent) => worker.onmessage!({ data: { engine, event } })
    try {
        transport.begin(engine, 100)
        deliver({ kind: 'execution', running: true })
        // A delayed UI timer runs before the completion already queued by the Worker.
        now = 7_000
        vi.advanceTimersByTime(250)
        expect(failures).toEqual([])
        if (stallDuringProbe) {
            now = 14_000
            vi.advanceTimersByTime(250)
            expect(failures).toEqual([])
        }
        deliver({ kind: 'execution', running: false })
        deliver({ kind: 'result', sequence: 1, value: 'completed' })
        expect(received).toEqual([{ kind: 'result', sequence: 1, value: 'completed' }])
        transport.end(engine)
        now += 2_000
        vi.advanceTimersByTime(2_000)
        expect(failures).toEqual([])
    } finally {
        transport.release(engine)
        clock.mockRestore()
        vi.useRealTimers()
    }
})
