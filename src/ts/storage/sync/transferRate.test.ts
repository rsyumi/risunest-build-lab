import { expect, it } from 'vitest'
import { createRateMeter } from './transferRate'

it('lets an idle view decay without adding invented samples to the transfer history', () => {
    const meter = createRateMeter()
    meter.add(0, 0); meter.add(1000, 1000)
    expect(meter.rate()).toBe(1000)
    expect(meter.rate(2000)).toBe(500)
    expect(meter.rate(4000)).toBe(0)
    expect(meter.rate()).toBe(1000)
    meter.add(2000, 2000)
    expect(meter.rate()).toBe(1000)
    meter.reset()
    expect(meter.rate(5000)).toBeUndefined()
})
