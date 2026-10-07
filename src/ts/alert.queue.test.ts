// @vitest-environment happy-dom
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { get } from 'svelte/store'

const markers = vi.hoisted(() => ({ values: new Map<string, string>() }))
vi.mock('./storage/deviceMarkers', () => ({
    getDeviceMarkers: () => ({
        getItem: (key: string) => markers.values.get(key) ?? null,
        setItem: (key: string, value: string) => { markers.values.set(key, value) },
        flush: async () => {},
    }),
}))
vi.mock('./stores.svelte', async () => {
    const { createAlertQueue } = await import('./alertQueue')
    return { alertStore: createAlertQueue({ type: 'none', msg: '' }, { gapMs: 0 }) }
})
vi.mock('./storage/database.svelte', () => ({ getDatabase: vi.fn(() => ({})) }))
vi.mock('src/ts/platform', () => ({ isTauri: false }))
vi.mock('../lang', () => ({ language: {} }))

import { alertStore } from './stores.svelte'
import { alertClear, alertConfirm, alertError, alertSelect, alertToast, alertTOS, alertWait, doingAlert, waitAlert } from './alert'

async function answer(msg: string) {
    await vi.advanceTimersByTimeAsync(0)
    alertStore.set({ type: 'none', msg })
    await vi.advanceTimersByTimeAsync(0)
}

beforeEach(() => {
    vi.useFakeTimers()
    vi.spyOn(console, 'error').mockImplementation(() => {})
    markers.values.clear()
})
afterEach(async () => {
    while (alertStore.hasDialogs()) await answer('')
    alertClear()
    vi.useRealTimers()
    vi.restoreAllMocks()
})

describe('alerts on the queue', () => {
    it('keeps the terms prompt open through a toast', async () => {
        const accepted = alertTOS()
        alertToast('Copied')
        expect(get(alertStore).type).toBe('tos')
        await answer('yes')
        await expect(accepted).resolves.toBe(true)
        expect(markers.values.get('risunest_tos_v1')).toBe('true')
        expect(get(alertStore)).toEqual({ type: 'toast', msg: 'Copied' })
    })

    it('answers overlapping questions separately and in order', async () => {
        const first = alertConfirm('Delete the chat?')
        const second = alertSelect(['Keep', 'Replace'])
        expect(get(alertStore)).toMatchObject({ type: 'ask', msg: 'Delete the chat?' })
        await answer('yes')
        expect(get(alertStore)).toMatchObject({ type: 'select', msg: 'Keep||Replace' })
        await answer('1')
        await expect(first).resolves.toBe(true)
        await expect(second).resolves.toBe('1')
    })

    it('shows an error raised during a question after the question closes', async () => {
        const question = alertConfirm('Continue?')
        alertError('synthetic failure')
        await answer('no')
        await expect(question).resolves.toBe(false)
        expect(get(alertStore)).toMatchObject({ type: 'error', msg: 'synthetic failure' })
    })

    it('leaves a dialog in place when a finished task clears its loading overlay', async () => {
        alertWait('Loading')
        const question = alertConfirm('Overwrite?')
        alertWait('Saving')
        alertClear()
        expect(get(alertStore)).toMatchObject({ type: 'ask' })
        await answer('yes')
        await expect(question).resolves.toBe(true)
        expect(get(alertStore).type).toBe('none')
    })

    it('counts the wait before the next queued dialog as an open alert', async () => {
        void alertConfirm('One?')
        void alertConfirm('Two?')
        alertStore.set({ type: 'none', msg: 'yes' })
        expect(get(alertStore).type).toBe('none')
        expect(doingAlert()).toBe(true)
        await answer('yes')
        expect(doingAlert()).toBe(false)
    })

    it('waits for every queued dialog in waitAlert', async () => {
        void alertConfirm('One?')
        void alertConfirm('Two?')
        let done = false
        void waitAlert().then(() => { done = true })
        await answer('yes')
        expect(done).toBe(false)
        await answer('yes')
        expect(done).toBe(true)
    })
})
