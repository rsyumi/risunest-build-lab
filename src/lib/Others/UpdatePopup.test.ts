import { afterEach, describe, expect, it, vi } from 'vitest'
import { mount, tick, unmount } from 'svelte'
import { appUpdateState, initialAppUpdateState } from 'src/ts/update/state.svelte'
import { registerWindowCloseDrain } from 'src/ts/storage/syncExitProduction'
import { languageEnglish } from 'src/lang/en'
import UpdatePopup from './UpdatePopup.svelte'

const native = vi.hoisted(() => ({ close: vi.fn() }))
vi.mock('@tauri-apps/api/window', () => ({ getCurrentWindow: () => native }))
vi.mock('src/lang', async () => ({ language: (await import('src/lang/en')).languageEnglish }))
vi.mock('src/ts/storage/database.svelte', () => ({ getDatabase: () => ({ language: 'en' }) }))
vi.mock('src/ts/globalApi.svelte', () => ({ openURL: vi.fn() }))
vi.mock('src/ts/update/controller', () => ({
    applyAppUpdate: vi.fn(), cancelAppUpdateDownload: vi.fn(),
    dismissAppUpdate: vi.fn(), skipAppUpdate: vi.fn(),
}))

let component: ReturnType<typeof mount> | undefined
let target: HTMLDivElement | undefined
afterEach(async () => {
    if (component) await unmount(component)
    target?.remove()
    appUpdateState.set({ ...initialAppUpdateState })
    native.close.mockReset()
})

describe('staged Linux update exit', () => {
    it('uses the close coordinator and preserves a cancelled decision', async () => {
        let closeHandler!: (event: { preventDefault(): void }) => void | Promise<void>
        const destroy = vi.fn(async () => {})
        let finish!: (disposition: 'exit' | 'cancelled') => void
        const requestExit = vi.fn(() => new Promise<'exit' | 'cancelled'>(resolve => { finish = resolve }))
        await registerWindowCloseDrain({
            onCloseRequested: async handler => { closeHandler = handler; return () => {} }, destroy,
        }, { requestExit } as never)
        native.close.mockImplementation(async () => closeHandler({ preventDefault: vi.fn() }))
        appUpdateState.set({
            ...initialAppUpdateState, phase: 'staged', popupVisible: true,
            update: { handleId: 'synthetic', version: '1.0.0', pubDate: '2026-01-01', notes: '', localizedNotes: {},
                releasePage: '', installStrategy: 'stage-deb', downloadUrl: '', downloadSize: 1, format: 'deb' },
            stagedDeb: { path: '/synthetic/update.deb', installCommand: 'synthetic install' },
        })
        target = document.createElement('div')
        document.body.append(target)
        component = mount(UpdatePopup, { target })
        await tick()
        const exit = [...target.querySelectorAll('button')].find(button => button.textContent?.trim() === languageEnglish.risuNest.update.exitApp)!
        exit.click()
        await tick()
        expect(requestExit).toHaveBeenCalledOnce()
        expect(destroy).not.toHaveBeenCalled()
        finish('cancelled')
        await native.close.mock.results[0].value
        expect(destroy).not.toHaveBeenCalled()
        exit.click()
        finish('exit')
        await native.close.mock.results[1].value
        expect(destroy).toHaveBeenCalledOnce()
    })
})
