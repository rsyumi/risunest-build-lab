// @vitest-environment happy-dom
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { get } from 'svelte/store'

const markers = vi.hoisted(() => ({ values: new Map<string, string>(), flush: vi.fn() }))
vi.mock('./storage/deviceMarkers', () => ({
    getDeviceMarkers: () => ({
        getItem: (key: string) => markers.values.get(key) ?? null,
        setItem: (key: string, value: string) => { markers.values.set(key, value) },
        removeItem: (key: string) => { markers.values.delete(key) },
        flush: markers.flush,
    }),
}))
vi.mock('./stores.svelte', async () => {
    const { writable } = await import('svelte/store')
    return { alertStore: writable({ type: 'none', msg: '' }) }
})
vi.mock('./storage/database.svelte', () => ({ getDatabase: vi.fn(() => ({})) }))
vi.mock('./util', () => ({ sleep: (milliseconds: number) => new Promise(resolve => setTimeout(resolve, milliseconds)) }))
vi.mock('src/ts/platform', () => ({ isTauri: false }))
vi.mock('../lang', () => ({ language: {} }))

import { alertStore } from './stores.svelte'
import { alertLogin, openRisuAccountLogin } from './alert'

beforeEach(() => {
    vi.useFakeTimers()
    markers.values.clear()
    markers.flush.mockReset().mockResolvedValue(undefined)
    alertStore.set({ type: 'none', msg: '' })
})
afterEach(() => { vi.useRealTimers() })

async function answer(accepted: boolean) {
    alertStore.set({ type: 'none', msg: accepted ? 'yes' : 'no' })
    await vi.advanceTimersByTimeAsync(10)
}

describe('account service consent', () => {
    it('does not open account login when service consent is declined, even with app terms accepted', async () => {
        markers.values.set('risunest_tos_v1', 'true')
        const open = vi.fn()
        const result = openRisuAccountLogin(open)
        expect(get(alertStore).type).toBe('risu-tos')
        expect(open).not.toHaveBeenCalled()
        await answer(false)
        await expect(result).resolves.toBe(false)
        expect(open).not.toHaveBeenCalled()
        expect(markers.values.has('risu_service_tos_v1')).toBe(false)
        expect(markers.flush).not.toHaveBeenCalled()
    })

    it('opens only after service acceptance is durably flushed', async () => {
        let finish!: () => void
        markers.flush.mockImplementationOnce(() => new Promise<void>(resolve => { finish = resolve }))
        const open = vi.fn()
        const result = openRisuAccountLogin(open)
        await answer(true)
        expect(markers.values.get('risu_service_tos_v1')).toBe('true')
        expect(markers.values.has('risunest_tos_v1')).toBe(false)
        expect(markers.flush).toHaveBeenCalledOnce()
        expect(open).not.toHaveBeenCalled()
        finish()
        await expect(result).resolves.toBe(true)
        expect(open).toHaveBeenCalledOnce()
    })

    it('reuses existing service acceptance without showing another dialog', async () => {
        markers.values.set('risu_service_tos_v1', 'true')
        const open = vi.fn()
        await expect(openRisuAccountLogin(open)).resolves.toBe(true)
        expect(open).toHaveBeenCalledOnce()
        expect(get(alertStore)).toEqual({ type: 'none', msg: '' })
        expect(markers.flush).not.toHaveBeenCalled()
    })

    it('does not open login after consent persistence fails', async () => {
        markers.flush.mockRejectedValueOnce(new Error('synthetic marker write failed'))
        const open = vi.fn()
        const result = openRisuAccountLogin(open)
        const rejected = expect(result).rejects.toThrow('synthetic marker write failed')
        await answer(true)
        await rejected
        expect(open).not.toHaveBeenCalled()
    })

    it('keeps reauthentication closed after a declined service gate', async () => {
        const result = alertLogin()
        expect(get(alertStore).type).toBe('risu-tos')
        await answer(false)
        await expect(result).resolves.toBe('')
        expect(get(alertStore).type).toBe('none')
    })

    it.each([false, true])('gates reauthentication and returns its result (existing consent: %s)', async accepted => {
        if (accepted) markers.values.set('risu_service_tos_v1', 'true')
        const result = alertLogin()
        if (!accepted) {
            expect(get(alertStore).type).toBe('risu-tos')
            await answer(true)
        } else {
            await vi.advanceTimersByTimeAsync(0)
        }
        expect(get(alertStore)).toEqual({ type: 'login', msg: 'login' })
        alertStore.set({ type: 'none', msg: 'synthetic-login-result' })
        await vi.advanceTimersByTimeAsync(10)
        await expect(result).resolves.toBe('synthetic-login-result')
    })
})
