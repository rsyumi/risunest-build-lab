import { expect, test } from 'vitest'
import {
    displayListenerEngine,
    nextDisplayListenerOrder,
    noteDisplayListenerTimeout,
    skipsDisplayListener,
} from './luaDisplayTimeouts'

const request = (index: number) => ({
    order: nextDisplayListenerOrder(),
    row: { conversationId: 'conversation', index, messageId: `message-${index}` },
})

test('forgets the least recently skipped rows beyond its bound', () => {
    const engine = displayListenerEngine('rows', 'code')
    noteDisplayListenerTimeout(engine, request(0), 'input')
    for (let index = 1; index < 1_000; index++) {
        noteDisplayListenerTimeout(engine, request(index), 'input')
        expect(skipsDisplayListener(engine, request(0), 'input')).toBe(true)
    }
    expect(skipsDisplayListener(engine, request(1), 'input')).toBe(false)
    expect(skipsDisplayListener(engine, request(999), 'input')).toBe(true)
})

test('forgets the requests to skip of the oldest timed-out engines beyond its bound', () => {
    const engine = displayListenerEngine('engines', 'code')
    const waiting = { order: nextDisplayListenerOrder() }
    noteDisplayListenerTimeout(engine, { order: nextDisplayListenerOrder() }, 'input')
    expect(skipsDisplayListener(engine, waiting, 'input')).toBe(true)
    for (let index = 0; index < 1_000; index++) {
        noteDisplayListenerTimeout(displayListenerEngine('engines', `code ${index}`), { order: nextDisplayListenerOrder() }, 'input')
    }
    expect(skipsDisplayListener(engine, waiting, 'input')).toBe(false)
})
