import { expect, test } from 'vitest'
import { createLuaHistoryJSON } from './luaHistoryJSON'
import type { Message } from '../storage/database.svelte'

test('sees direct field edits, replaced windows and appended/removed messages', () => {
    const encode = createLuaHistoryJSON()
    const messages: Message[] = [{ role: 'user', data: 'first', chatId: 'a' }]
    const expected = () => JSON.stringify(messages.map(row => ({
        role: row.role, data: row.data, chatId: row.chatId, time: row.time ?? 0,
    })))
    expect(encode(messages)).toBe(expected())
    messages[0].data = 'edited'
    expect(encode(messages)).toBe(expected())
    messages[0].role = 'char'
    messages[0].time = 12
    messages[0].chatId = 'b'
    expect(encode(messages)).toBe(expected())
    messages.push({ role: 'char', data: 'second' })
    expect(encode(messages)).toBe(expected())
    messages.shift()
    expect(encode(messages)).toBe(expected())
    expect(encode([])).toBe('[]')
})

test('large values and custom serialization keep producing current output', () => {
    const encode = createLuaHistoryJSON(8)
    const messages: Message[] = [{ role: 'user', data: 'large' }]
    expect(JSON.parse(encode(messages))[0].data).toBe('large')
    messages[0].data = 'changed'
    expect(JSON.parse(encode(messages))[0].data).toBe('changed')
    let value = 'a'
    messages[0].data = { toJSON: () => value } as never
    expect(JSON.parse(encode(messages))[0].data).toBe('a')
    value = 'b'
    expect(JSON.parse(encode(messages))[0].data).toBe('b')
})

test('does not reuse rows removed from a sparse message array', () => {
    const encode = createLuaHistoryJSON()
    const messages: Message[] = [{ role: 'user', data: 'removed' }]
    encode(messages)
    delete messages[0]
    expect(encode(messages)).toBe('[null]')
    messages[0] = { role: 'char', data: 'restored' }
    expect(JSON.parse(encode(messages))[0].data).toBe('restored')
})
