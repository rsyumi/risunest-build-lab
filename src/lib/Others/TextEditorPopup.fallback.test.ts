// @vitest-environment happy-dom

import { afterEach, describe, expect, it, vi } from 'vitest'
import { mount, tick, unmount } from 'svelte'

vi.mock('src/ts/platform', () => ({ isMobile: false }))
vi.mock('src/ts/stores.svelte', async () => {
    const { writable } = await import('svelte/store')
    return { alertStore: writable({ type: 'none', msg: '' }) }
})
vi.mock('./TextEditorMonaco.svelte', () => {
    throw new Error('Synthetic chunk load failure')
})

import TextEditorPopup from './TextEditorPopup.svelte'
import { openTextEditorPopup, textEditorPopup, type TextEditorPopupRequest } from 'src/ts/gui/textEditorPopup.svelte'

let component: ReturnType<typeof mount> | undefined

afterEach(async () => {
    if (component) await unmount(component)
    component = undefined
    textEditorPopup.request = null
    document.body.replaceChildren()
})

describe('text editor popup without the code editor', () => {
    it('falls back to the plain editor and focuses it when the code editor fails to load', async () => {
        const request: TextEditorPopupRequest = { value: 'Original text', save: vi.fn(() => true) }
        openTextEditorPopup(request)
        const target = document.createElement('div')
        document.body.append(target)
        component = mount(TextEditorPopup, { target, props: { request } })
        await tick()

        const textarea = await vi.waitFor(() => {
            const element = target.querySelector('textarea')
            expect(element).not.toBeNull()
            return element!
        })
        expect(textarea.value).toBe('Original text')
        await vi.waitFor(() => expect(document.activeElement).toBe(textarea))
        expect(target.textContent).not.toContain('Loading')
    })
})
