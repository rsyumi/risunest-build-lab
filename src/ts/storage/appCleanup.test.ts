import { beforeEach, describe, expect, it, vi } from 'vitest'
import { invoke } from '@tauri-apps/api/core'
import { requestAppCleanup } from './appCleanup'

vi.mock('@tauri-apps/api/core', () => ({ invoke: vi.fn() }))

describe('app cleanup request', () => {
    beforeEach(() => vi.clearAllMocks())
    const options = () => ({ native: true, desktop: true, prepareRemoval: false, confirm: vi.fn().mockResolvedValue(true) })
    it('does not invoke native cleanup after cancellation', async () => {
        const request = options()
        request.confirm.mockResolvedValue(false)
        expect(await requestAppCleanup(request)).toBe(false)
        expect(invoke).not.toHaveBeenCalled()
    })
    it.each([false, true])('passes removal choice %s to the native coordinator', async prepareRemoval => {
        expect(await requestAppCleanup({ ...options(), prepareRemoval })).toBe(true)
        expect(invoke).toHaveBeenCalledWith('app_cleanup_request', { mode: prepareRemoval ? 'prepare-removal' : 'reset' })
        expect(invoke).toHaveBeenCalledTimes(1)
    })
    it('does not prompt or invoke native cleanup on web', async () => {
        const request = { ...options(), native: false }
        expect(await requestAppCleanup(request)).toBe(false)
        expect(request.confirm).not.toHaveBeenCalled()
        expect(invoke).not.toHaveBeenCalled()
    })
    it('allows mobile reset and rejects removal preparation', async () => {
        await requestAppCleanup({ ...options(), desktop: false })
        expect(invoke).toHaveBeenCalledWith('app_cleanup_request', { mode: 'reset' })
        vi.clearAllMocks()
        await expect(requestAppCleanup({ ...options(), desktop: false, prepareRemoval: true })).rejects.toThrow('app-cleanup-mode-unavailable')
        expect(invoke).not.toHaveBeenCalled()
    })
})
