import { afterEach, expect, it, vi } from 'vitest'
import { accountReadDeadline, readAccountBody } from './accountReadBody'

afterEach(() => vi.useRealTimers())
it('times out headers and resets the idle deadline when body chunks arrive', async () => {
    vi.useFakeTimers()
    const headers = accountReadDeadline(undefined, 100)
    await vi.advanceTimersByTimeAsync(100)
    expect(headers.signal.reason).toMatchObject({ kind: 'timeout' }); headers.dispose()
    const deadline = accountReadDeadline(undefined, 100)
    await vi.advanceTimersByTimeAsync(90)
    let stream!: ReadableStreamDefaultController<Uint8Array>
    const body = new ReadableStream<Uint8Array>({ start(controller) { stream = controller } })
    const received: number[] = []
    const read = readAccountBody(new Response(body), deadline.signal, deadline.reset, count => received.push(count))
    for (let i = 0; i < 3; i++) { await vi.advanceTimersByTimeAsync(90); stream.enqueue(Uint8Array.of(i)); await vi.advanceTimersByTimeAsync(0) }
    expect(deadline.signal.aborted).toBe(false)
    stream.close()
    expect(await read).toEqual(Uint8Array.of(0, 1, 2)); expect(received).toEqual([1, 1, 1]); deadline.dispose()
})
it('rejects a stalled or truncated body without returning partial bytes', async () => {
    vi.useFakeTimers()
    const deadline = accountReadDeadline(undefined, 100)
    const read = readAccountBody(new Response(new ReadableStream()), deadline.signal, deadline.reset)
    const rejected = expect(read).rejects.toMatchObject({ kind: 'timeout' })
    await vi.advanceTimersByTimeAsync(100); await rejected; deadline.dispose()
    await expect(readAccountBody(new Response(Uint8Array.of(1), { headers: { 'content-length': '2' } }),
        new AbortController().signal, () => {})).rejects.toMatchObject({ kind: 'malformed' })
    await expect(readAccountBody(new Response(null, { headers: { 'content-length': '2' } }),
        new AbortController().signal, () => {})).rejects.toMatchObject({ kind: 'malformed' })
})
