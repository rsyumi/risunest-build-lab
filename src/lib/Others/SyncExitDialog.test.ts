import { afterEach, describe, expect, it, vi } from 'vitest'
import { mount, tick, unmount } from 'svelte'
import { languageEnglish } from 'src/lang/en'
import { configureSyncExitCoordinator, syncExitDialogState } from 'src/ts/storage/syncExitProduction'
import type { SyncExitDecision } from 'src/ts/storage/syncExitCoordinator'
import SyncExitDialog from './SyncExitDialog.svelte'
import { installAppLockInput, registerAppLockSurface, setAppLockInputBlocked } from 'src/ts/appLock/input'


let component: ReturnType<typeof mount> | undefined
let target: HTMLDivElement | undefined
afterEach(async () => {
    setAppLockInputBlocked(false)
    registerAppLockSurface(null)
    if (component) await unmount(component)
    target?.remove()
    syncExitDialogState.set({ phase: 'idle' })
})

async function show(aboveAppLock = false) {
    target = document.createElement('div')
    document.body.append(target)
    component = mount(SyncExitDialog, { target, props: { aboveAppLock } })
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
            expect(element.querySelector('details pre')?.textContent).toContain('RisuNest Version:')
            expect(element.querySelector('details pre')?.textContent).toContain(reason)
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

it('routes keyboard exclusively to the exit dialog above the app lock', async () => {
    const uninstall = installAppLockInput()
    let choice: SyncExitDecision | undefined
    configureSyncExitCoordinator({
        requestExit: async () => 'cancelled', requestExitWithoutSync: () => false,
        decide: value => { choice = value; syncExitDialogState.set({ phase: 'cancelled', target: null }); return true },
        snapshot: () => ({ phase: 'idle' }),
        subscribe: listener => { listener({ phase: 'idle' }); return () => {} },
    })
    const gate = document.createElement('div')
    gate.tabIndex = -1
    document.body.append(gate)
    let pin = ''
    registerAppLockSurface({ element: gate, keydown: event => { pin += event.key } })
    setAppLockInputBlocked(true)
    syncExitDialogState.set({ phase: 'remote-blocked', target: null, destination: 'synthetic', reason: 'server-sync-failed' })
    const element = await show(true)
    const panel = element.querySelector<HTMLElement>('[role="dialog"]')!
    panel.focus()
    panel.dispatchEvent(new KeyboardEvent('keydown', { key: '1', bubbles: true, cancelable: true }))
    panel.dispatchEvent(new KeyboardEvent('keydown', { key: 'Tab', bubbles: true, cancelable: true }))
    expect(panel.contains(document.activeElement)).toBe(true)
    expect(pin).toBe('')
    panel.dispatchEvent(new KeyboardEvent('keydown', { key: 'Escape', bubbles: true, cancelable: true }))
    await tick()
    expect(choice).toBe('cancel-exit')
    expect(element.querySelector('[role="dialog"]')).toBeNull()
    await tick()
    expect(document.activeElement).toBe(gate)
    gate.dispatchEvent(new KeyboardEvent('keydown', { key: '2', bubbles: true, cancelable: true }))
    expect(pin).toBe('2')
    gate.remove()
    uninstall()
})
