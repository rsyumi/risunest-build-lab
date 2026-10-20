import { expect, it } from 'vitest'
import { withoutAbortSignalAny } from '../../tests/support/abortSignalAny'
import { joinAbortSignals } from './abortSignals'

withoutAbortSignalAny()

it.each([0, 1])('aborts with the reason of input %i when that input aborts first', index => {
    const inputs = [new AbortController(), new AbortController()]
    const joined = joinAbortSignals(inputs.map(input => input.signal))
    expect(joined.signal.aborted).toBe(false)
    inputs[index].abort()
    expect(joined.signal.aborted).toBe(true)
    expect(joined.signal.reason).toBe(inputs[index].signal.reason)
    expect(joined.signal.reason).toMatchObject({ name: 'AbortError' })
    inputs[1 - index].abort('later')
    expect(joined.signal.reason).toBe(inputs[index].signal.reason)
})

it('starts aborted with the reason of the first input that is already aborted', () => {
    const live = new AbortController(), first = new AbortController(), second = new AbortController()
    first.abort('first'); second.abort('second')
    const joined = joinAbortSignals([live.signal, undefined, first.signal, second.signal])
    expect(joined.signal.aborted).toBe(true)
    expect(joined.signal.reason).toBe('first')
})

it('stops following its inputs once disposed', () => {
    const input = new AbortController()
    const joined = joinAbortSignals([input.signal, null])
    joined.dispose(); joined.dispose()
    input.abort('late')
    expect(joined.signal.aborted).toBe(false)
})
