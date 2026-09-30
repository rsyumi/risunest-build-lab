import { expect, it, vi } from 'vitest'
import { TaskRateLimiter } from './taskRateLimiter'

it('settles queued work immediately on cancellation without starting later tasks', async () => {
    const limiter = new TaskRateLimiter({ maxConcurrentTasks: 1, tasksPerMinute: 10 })
    const abort = new AbortController()
    let finish!: (value: string) => void
    const active = new Promise<string>(resolve => { finish = resolve })
    const first = vi.fn(() => active)
    const next = vi.fn(async () => 'must not run')
    const batch = limiter.executeBatch([first, next, next], abort.signal)
    expect(first).toHaveBeenCalledOnce()
    abort.abort()
    expect(limiter.queuedTaskCount).toBe(0)
    finish('already completed')
    const result = await batch
    expect(result.successCount).toBe(1)
    expect(result.failureCount).toBe(2)
    expect(next).not.toHaveBeenCalled()
})
