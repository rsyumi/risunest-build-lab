import { describe, expect, it, vi } from 'vitest'
const invoke = vi.hoisted(() => vi.fn())
vi.mock('@tauri-apps/api/core', () => ({ invoke }))
import { relaunch } from './desktopRelaunch'

describe('desktop restart', () => {
    it('uses the native restart with launch file operands removed', async () => {
        invoke.mockResolvedValueOnce(undefined)
        await relaunch()
        expect(invoke).toHaveBeenCalledWith('desktop_relaunch')
        invoke.mockRejectedValueOnce(new Error('restart unavailable'))
        await expect(relaunch()).rejects.toThrow('restart unavailable')
    })
})
