import { describe, expect, test } from 'vitest'
import {
    beginDisplayWork,
    createDisplayRecovery,
    installDisplayRecovery,
    listenForDisplayWorkResume,
    suspendDisplayWork,
} from './displayRecovery.svelte'
import { manualTimers, recoveryStore } from './displayRecovery.testSupport'

const first = { characterId: 'character', conversationId: 'first' }
const second = { characterId: 'character', conversationId: 'second' }

function settled(promise: Promise<void> | null) {
    return Promise.race([
        promise?.then(() => true) ?? Promise.resolve(true),
        new Promise<boolean>((resolve) => setTimeout(() => resolve(false), 0)),
    ])
}

describe('display work record', () => {
    test('writes the record once per chat and waits only while that write is pending', async () => {
        const device = recoveryStore()
        const recovery = createDisplayRecovery(device.store, { timers: manualTimers().timers })
        const row = recovery.beginWork(first)
        const nextRow = recovery.beginWork(first)
        expect(device.records).toEqual([{ working: [first], marked: [] }])
        expect(row.ready).not.toBeNull()
        expect(nextRow.ready).toBe(row.ready)
        expect(await settled(row.ready)).toBe(false)

        // Work of another chat waits for its own write after the earlier one is stored.
        const otherRow = recovery.beginWork(second)
        device.commitNext()
        expect(await settled(row.ready)).toBe(true)
        expect(recovery.beginWork(second).ready).toBe(otherRow.ready)
        expect(await settled(otherRow.ready)).toBe(false)

        device.commitAll()
        expect(await settled(otherRow.ready)).toBe(true)
        expect(recovery.beginWork(first).ready).toBeNull()
        expect(device.records).toHaveLength(2)
    })

    test('removes the record a while after the last work ends, unless work starts again', async () => {
        const device = recoveryStore()
        const clock = manualTimers()
        const recovery = createDisplayRecovery(device.store, { timers: clock.timers })
        const replaced = recovery.beginWork(first)
        const replacement = recovery.beginWork(first)
        replaced.release()
        replaced.release()
        expect(clock.pending).toBe(0)

        replacement.release()
        expect(clock.pending).toBe(1)
        const restarted = recovery.beginWork(first)
        expect(clock.pending).toBe(0)
        restarted.release()
        clock.runAll()
        expect(device.records).toEqual([{ working: [first], marked: [] }, null])

        device.commitAll()
        const again = recovery.beginWork(first)
        expect(again.ready).not.toBeNull()
        expect(device.records.at(-1)).toEqual({ working: [first], marked: [] })
    })

    test('removes the record in the background and writes it again for work that continued', () => {
        const device = recoveryStore()
        const recovery = createDisplayRecovery(device.store, { timers: manualTimers().timers })
        const stream = recovery.beginWork(first)
        recovery.beginWork(second).release()
        void recovery.suspend()
        expect(device.records.at(-1)).toBeNull()

        expect(recovery.beginWork(second).ready).toBeNull()
        expect(device.records.at(-1)).toBeNull()
        stream.release()
        recovery.resume()
        expect(device.records.at(-1)).toEqual({ working: [second], marked: [] })
    })
})

describe('display work while the app is away', () => {
    test('removes the record when the app goes to the background and writes it again on return', async () => {
        const device = recoveryStore(null, { commitAtOnce: true })
        await installDisplayRecovery(device.store, { timers: manualTimers().timers })
        const stopListening = listenForDisplayWorkResume()
        let visibility: DocumentVisibilityState = 'visible'
        Object.defineProperty(document, 'visibilityState', { configurable: true, get: () => visibility })
        try {
            const stream = beginDisplayWork(first)
            // A memory warning can arrive while the app is in the foreground.
            await suspendDisplayWork('trim-memory')
            expect(device.record).toEqual({ working: [first], marked: [] })

            visibility = 'hidden'
            await suspendDisplayWork('visibility-hidden')
            expect(device.record).toBeNull()
            window.dispatchEvent(new Event('focus'))
            expect(device.record).toBeNull()

            visibility = 'visible'
            document.dispatchEvent(new Event('visibilitychange'))
            expect(device.record).toEqual({ working: [first], marked: [] })
            stream.release()
        } finally {
            stopListening()
            delete (document as { visibilityState?: DocumentVisibilityState }).visibilityState
            await installDisplayRecovery(null)
        }
    })
})

describe('recovery marks', () => {
    test('marks the chats the last run left in the record and keeps the newest within the bound', () => {
        const older = Array.from({ length: 3 }, (_, index) => ({ characterId: 'character', conversationId: `older-${index}` }))
        const device = recoveryStore(JSON.stringify({ working: [first], marked: [...older, first] }))
        const recovery = createDisplayRecovery(device.store, { markLimit: 3, timers: manualTimers().timers })
        expect(device.records).toEqual([{ working: [], marked: [older[1], older[2], first] }])
        expect(recovery.isMarked(first)).toBe(true)
        expect(recovery.isMarked(older[0])).toBe(false)
        expect(recovery.isMarked(second)).toBe(false)
    })

    test('keeps a mark across launches until it is forgotten', () => {
        const device = recoveryStore(JSON.stringify({ working: [first], marked: [] }))
        createDisplayRecovery(device.store)
        const relaunched = createDisplayRecovery(device.store)
        expect(device.records).toHaveLength(1)
        expect(relaunched.isMarked(first)).toBe(true)

        void relaunched.forget((target) => target.conversationId === first.conversationId)
        expect(relaunched.isMarked(first)).toBe(false)
        expect(device.records.at(-1)).toBeNull()
    })

    test('starts without marks from an unreadable record and removes it', () => {
        const device = recoveryStore('{')
        const recovery = createDisplayRecovery(device.store)
        expect(recovery.isMarked(first)).toBe(false)
        expect(device.records).toEqual([null])
    })
})
