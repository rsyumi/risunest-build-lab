import { afterEach, expect, it, vi } from 'vitest'
import { get } from 'svelte/store'
import { notifySyncBindingChanged } from './bindingChanges'
import type { BindingTarget } from './bindingFlow'
import { installSyncStatus, syncStatus, type SyncStatusSource } from './syncStatus'

let uninstall = () => {}
afterEach(() => uninstall())

it('follows the bound target, ignores a stale read, and draws nothing without a binding', async () => {
    const reads: { resolve(target: BindingTarget): void }[] = []
    const stopped: string[] = []
    const server = { target: 'server' } as SyncStatusSource
    const external = { target: 'external' } as SyncStatusSource
    uninstall = installSyncStatus({
        target: () => new Promise(resolve => reads.push({ resolve })),
        server(emit) { emit(server); return () => stopped.push('server') },
        external(connectionId, emit) { emit(external); return () => stopped.push(connectionId) },
    })
    reads[0].resolve({ kind: 'none' })
    await new Promise(resolve => setTimeout(resolve, 0))
    expect(get(syncStatus)).toBeNull()

    notifySyncBindingChanged()
    reads[1].resolve({ kind: 'server', connectionId: 'server' })
    await vi.waitFor(() => expect(get(syncStatus)).toBe(server))

    // A binding change while the target is read wins over the older read.
    notifySyncBindingChanged()
    notifySyncBindingChanged()
    reads[3].resolve({ kind: 'external', connectionId: 'webdav-1' })
    await vi.waitFor(() => expect(get(syncStatus)).toBe(external))
    reads[2].resolve({ kind: 'none' })
    await new Promise(resolve => setTimeout(resolve, 0))
    expect(get(syncStatus)).toBe(external)
    expect(stopped).toEqual(['server'])

    notifySyncBindingChanged()
    reads[4].resolve({ kind: 'none' })
    await vi.waitFor(() => expect(get(syncStatus)).toBeNull())
    expect(stopped).toEqual(['server', 'webdav-1'])
})
