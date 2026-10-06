import { afterEach, describe, expect, it, vi } from 'vitest'
import { mount, tick, unmount } from 'svelte'
import { languageEnglish } from 'src/lang/en'
import { configureSyncExitCoordinator, syncExitDialogState } from 'src/ts/storage/syncExitProduction'
import type { SyncExitDecision } from 'src/ts/storage/syncExitCoordinator'
import SyncExitDialog from './SyncExitDialog.svelte'

vi.mock('src/lang', async () => ({
    language: (await import('src/lang/en')).languageEnglish,
}))

let component: ReturnType<typeof mount> | undefined
let target: HTMLDivElement | undefined
afterEach(async () => {
    if (component) await unmount(component)
    target?.remove()
    syncExitDialogState.set({ phase: 'idle' })
})

async function show() {
    target = document.createElement('div')
    document.body.append(target)
    component = mount(SyncExitDialog, { target })
    await tick()
    return target
}

describe('sync exit dialog', () => {
    it.each(['server-sync-failed', 'library-operation-busy'])(
        'shows %s inside the modal with an explicit retry action', async (reason) => {
            const decide = vi.fn((_choice: SyncExitDecision) => true)
            configureSyncExitCoordinator({
                requestExit: async () => 'cancelled', requestExitWithoutSync: () => false, decide,
                snapshot: () => ({ phase: 'idle' }),
                subscribe: listener => { listener({ phase: 'idle' }); return () => {} },
            })
            syncExitDialogState.set({
                phase: 'remote-blocked', target: null, destination: 'server:connection:epoch', reason,
            })
            const element = await show()
            const text = languageEnglish.risuNest
            expect(element.querySelector('[role="dialog"]')?.textContent).toContain(reason)
            expect(element.querySelector('#sync-exit-title')?.textContent).toBe(text.exitDrain.syncFailedTitle)
            expect(element.querySelector('#sync-exit-detail')?.textContent?.trim()).toBe(`${text.exitDrain.blocked} (${reason})`)
            const buttons = [...element.querySelectorAll('button')].map((b) => b.textContent?.trim())
            expect(buttons).toContain(text.exitDrain.retrySync)
            expect(buttons).toContain(text.exitDrain.cancelExit)
            expect(buttons).not.toContain(text.exitDrain.keepWaiting)
            element.querySelectorAll('button').forEach(button => {
                if (button.textContent?.trim() === text.exitDrain.retrySync) button.click()
            })
            expect(decide).toHaveBeenCalledExactlyOnceWith('wait')
        },
    )
    it('keeps exit and return available while continuing to wait without another wait prompt', async () => {
        syncExitDialogState.set({
            phase: 'remote-waiting', destination: 'server',
            target: { revision: 1, libraryEpoch: 'e', selectionEpoch: 's', selectionId: 'server' },
        })
        const element = await show()
        const buttons = [...element.querySelectorAll('button')].map((b) => b.textContent?.trim())
        expect(buttons).toEqual([
            languageEnglish.risuNest.exitDrain.cancelExit,
            languageEnglish.risuNest.exitDrain.exitWithoutSync,
        ])
    })
})
