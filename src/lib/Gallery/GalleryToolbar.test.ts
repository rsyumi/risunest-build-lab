import { afterEach, describe, expect, it } from 'vitest'
import { flushSync, mount, unmount } from 'svelte'
import GalleryToolbar from './GalleryToolbar.svelte'

let component: ReturnType<typeof mount> | undefined
afterEach(async () => {
    if (component) await unmount(component)
    component = undefined
    document.body.replaceChildren()
})
function render(selectedCount: number, total: number) {
    const calls: string[] = []
    const target = document.createElement('div')
    document.body.appendChild(target)
    component = mount(GalleryToolbar, { target, props: { selectedCount, total, onselectall: () => calls.push('select all'), onclear: () => calls.push('clear') } })
    flushSync()
    return { calls, box: target.querySelector<HTMLInputElement>('input[type="checkbox"]')! }
}

describe('gallery selection toolbar', () => {
    it('clears a complete selection from the select-all box', () => {
        const { calls, box } = render(3, 3)
        expect(box.checked).toBe(true)
        box.click()
        expect(calls).toEqual(['clear'])
        expect(box.checked).toBe(true)
    })
    it('selects everything from a partial selection and shows the selection until it changes', () => {
        const { calls, box } = render(1, 3)
        expect([box.checked, box.indeterminate]).toEqual([false, true])
        box.click()
        expect(calls).toEqual(['select all'])
        expect([box.checked, box.indeterminate]).toEqual([false, true])
    })
})
