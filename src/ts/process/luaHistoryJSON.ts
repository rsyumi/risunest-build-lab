import type { Message } from '../storage/database.svelte'

type Row = Pick<Message, 'role' | 'data' | 'chatId'> & { time: number }

/** Invocation-local cache; compares exposed fields so direct edits and window changes are visible. */
export function createLuaHistoryJSON(maxBytes = 2 * 1024 * 1024) {
    let previous: Row[] | undefined
    let encoded = ''
    return (messages: Message[]): string => {
        let changed = !previous || previous.length !== messages.length
        const rows = messages.map(message => ({
            role: message.role, data: message.data, chatId: message.chatId, time: message.time ?? 0,
        }))
        if (!changed) {
            for (let index = 0; index < rows.length; index++) {
                const row = rows[index]
                const old = previous![index]
                if (row === undefined ? old !== undefined : !old || row.role !== old.role || row.data !== old.data
                    || row.chatId !== old.chatId || !Object.is(row.time, old.time)) {
                    changed = true
                    break
                }
            }
        }
        if (!changed) return encoded
        const value = JSON.stringify(rows)
        // Retain only ordinary API values; custom toJSON objects must execute each time.
        if (value.length * 2 <= maxBytes && rows.every(row => typeof row.data === 'string'
            && typeof row.role === 'string' && (row.chatId === undefined || typeof row.chatId === 'string')
            && typeof row.time === 'number')) {
            previous = rows
            encoded = value
        } else {
            previous = undefined
            encoded = ''
        }
        return value
    }
}
