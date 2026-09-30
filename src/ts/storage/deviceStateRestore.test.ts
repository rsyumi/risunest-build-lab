import { beforeEach, describe, expect, it, vi } from 'vitest'

const state = vi.hoisted(() => ({
    flush: vi.fn(async () => {}), reload: vi.fn(async () => {}),
    settings: vi.fn(() => ({ nativeFileLogEnabled: false })), updates: vi.fn(),
    hub: vi.fn(), log: vi.fn(async () => {}),
}))
vi.mock('../platform', () => ({ isTauri: true }))
vi.mock('./deviceMarkers', () => ({ getDeviceMarkers: () => ({ flush: state.flush }), reloadDeviceMarkers: state.reload }))
vi.mock('./deviceSettings', () => ({ reloadDeviceSettings: state.settings }))
vi.mock('../update/settings', () => ({ reloadAppUpdateSettings: state.updates }))
vi.mock('../characterCards', () => ({ applyHubSelection: state.hub }))
vi.mock('../nativeLog', () => ({ setNativeLogFileEnabled: state.log }))
import { flushDeviceStateBeforeRestore, refreshDeviceStateAfterRestore } from './deviceStateRestore'

describe('restored device runtime state', () => {
    beforeEach(() => vi.clearAllMocks())
    it('flushes before restore and reapplies the restored hub and native log preference', async () => {
        await flushDeviceStateBeforeRestore()
        expect(state.flush).toHaveBeenCalledOnce()
        await refreshDeviceStateAfterRestore()
        expect(state.reload).toHaveBeenCalledOnce()
        expect(state.settings).toHaveBeenCalledOnce()
        expect(state.updates).toHaveBeenCalledOnce()
        expect(state.hub).toHaveBeenCalledOnce()
        expect(state.log).toHaveBeenCalledExactlyOnceWith(false)
        expect(state.reload.mock.invocationCallOrder[0]).toBeLessThan(state.settings.mock.invocationCallOrder[0])
    })
    it('keeps a failed marker reload from applying stale runtime values', async () => {
        state.reload.mockRejectedValueOnce(new Error('synthetic device read failure'))
        await expect(refreshDeviceStateAfterRestore()).rejects.toThrow('synthetic device read failure')
        expect(state.settings).not.toHaveBeenCalled()
        expect(state.hub).not.toHaveBeenCalled()
        expect(state.log).not.toHaveBeenCalled()
    })
})
