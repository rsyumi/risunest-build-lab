import { expect, test } from 'vitest'
import { LuaWorkerStrings } from './luaWorkerProtocol'

test('transmits changed strings, preserves exact values and recovers across bounded evictions', () => {
    const sender = new LuaWorkerStrings(16, 1)
    const receiver = new LuaWorkerStrings(16, 1)
    for (const value of ['first', 'first', 'second', 'first', '가나다', '', undefined, null, 0, false, 'oversized-value']) {
        expect(receiver.decode(structuredClone(sender.encode(value)))).toBe(value)
    }
    const value = 'same'
    const first = sender.encode(value)
    receiver.decode(first)
    const repeated = sender.encode(value)
    expect(repeated.value).toBeUndefined()
    expect(receiver.decode(repeated)).toBe(value)
    sender.clear()
    receiver.clear()
    expect(receiver.decode(sender.encode(value))).toBe(value)
})
