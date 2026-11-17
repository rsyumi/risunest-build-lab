import { expect, it, vi } from 'vitest'
import { trackAbortListeners, withoutAbortSignalAny } from '../../../tests/support/abortSignalAny'
vi.mock('@tauri-apps/api/core', () => ({ invoke: vi.fn() }))
import { runContentImport } from './contentImportOperation'
import { cancelActiveNativeFileOperation } from './nativeFileJobManager'

withoutAbortSignalAny()

const startImport = (caller: AbortSignal) => {
    let started!: () => void
    const running = new Promise<void>(resolve => { started = resolve })
    const result = runContentImport('synthetic.charx', { signal: caller }, ({ signal }) => new Promise<never>((_, reject) => {
        signal!.addEventListener('abort', () => reject(signal!.reason), { once: true })
        started()
    }))
    return { running, result }
}

it('stops an import when either its caller or the operation cancel action aborts it', async () => {
    const caller = new AbortController()
    const byCaller = startImport(caller.signal)
    await byCaller.running
    caller.abort('caller stopped')
    await expect(byCaller.result).rejects.toBe('caller stopped')
    const byCancel = startImport(new AbortController().signal)
    await byCancel.running
    cancelActiveNativeFileOperation()
    await expect(byCancel.result).rejects.toMatchObject({ name: 'AbortError' })
})

it('leaves no listener on the caller signal once an import settles', async () => {
    const caller = new AbortController()
    const attached = trackAbortListeners(caller.signal)
    await expect(runContentImport('synthetic.charx', { signal: caller.signal }, async () => 'imported')).resolves.toBe('imported')
    expect(attached()).toEqual([])
})
