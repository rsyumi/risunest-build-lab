import { afterEach, describe, expect, it, vi } from 'vitest'
import { mount, tick, unmount } from 'svelte'
import { languageEnglish } from 'src/lang/en'
import { syncExitDialogState } from 'src/ts/storage/syncExitProduction'
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
            syncExitDialogState.set({
                phase: 'remote-blocked', target: null, destination: 'server:connection:epoch', reason,
            })
            const element = await show()
            const text = languageEnglish.risuNest
            expect(element.querySelector('[role="dialog"]')?.textContent).toContain(reason)
            expect(element.textContent).toContain(reason === 'library-operation-busy'
                ? text.serverSync.busyHelp : text.serverSync.errorHelp)
            const buttons = [...element.querySelectorAll('button')].map((b) => b.textContent?.trim())
            expect(buttons).toContain(text.exitDrain.retrySync)
            expect(buttons).toContain(text.exitDrain.cancelExit)
            expect(buttons).not.toContain(text.exitDrain.keepWaiting)
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
