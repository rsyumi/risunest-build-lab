import { describe, expect, it, vi } from 'vitest'
import type { CommittedApplyOutcome } from './persistentDataRuntime'
import { retainableReplacementFence } from './retainableReplacementFence'

function physicalFence() {
    return {
        revision: 5,
        refreshCommittedWorkingSet: vi.fn(async (revision: number): Promise<CommittedApplyOutcome> => ({
            kind: 'committed', revision, projection: 'applied',
        })),
        release: vi.fn(),
    }
}

describe('retainable replacement fence', () => {
    it('releases the fence only when the last hold is released', () => {
        const physical = physicalFence()
        const fence = retainableReplacementFence(physical)
        const held = fence.retain()
        fence.release()
        fence.release()
        expect(physical.release).not.toHaveBeenCalled()
        held.release()
        held.release()
        expect(physical.release).toHaveBeenCalledOnce()
        expect(() => fence.retain()).toThrow(/released/)
    })

    it('refreshes through a live hold and refuses a released one', async () => {
        const physical = physicalFence()
        const fence = retainableReplacementFence(physical)
        const held = fence.retain()
        expect(held.revision).toBe(5)
        fence.release()
        await expect(fence.refreshCommittedWorkingSet(6)).rejects.toThrow(/released/)
        await expect(held.refreshCommittedWorkingSet(6)).resolves.toMatchObject({ revision: 6 })
        expect(physical.refreshCommittedWorkingSet).toHaveBeenCalledOnce()
    })
})
