import { afterEach, expect, it } from 'vitest'
import { tick } from 'svelte'
import { writable } from 'svelte/store'

import { restoreFocusAfterInputBlock } from './restoreFocusAfterInputBlock'

let stop: (() => void) | undefined

afterEach(() => {
    stop?.()
    stop = undefined
    document.body.replaceChildren()
})

function setup() {
    const blocked = writable(false)
    const editor = document.createElement('textarea')
    const other = document.createElement('input')
    document.body.append(editor, other)
    stop = restoreFocusAfterInputBlock(blocked)
    return { blocked, editor, other }
}

it('refocuses the editor that lost focus while input was blocked', async () => {
    const { blocked, editor } = setup()
    editor.value = 'draft'
    editor.focus()
    editor.setSelectionRange(2, 2)
    blocked.set(true)
    editor.blur()
    expect(document.activeElement).toBe(document.body)
    blocked.set(false)
    await tick()
    await Promise.resolve()
    expect(document.activeElement).toBe(editor)
    expect(editor.selectionStart).toBe(2)
})

it('leaves focus alone when the user moved it or the editor is gone', async () => {
    const { blocked, editor, other } = setup()
    editor.focus()
    blocked.set(true)
    other.focus()
    blocked.set(false)
    await tick()
    await Promise.resolve()
    expect(document.activeElement).toBe(other)

    other.focus()
    blocked.set(true)
    other.remove()
    blocked.set(false)
    await tick()
    await Promise.resolve()
    expect(document.activeElement).toBe(document.body)
})

it('only restores after the last block of a burst ends', async () => {
    const { blocked, editor } = setup()
    editor.focus()
    blocked.set(true)
    editor.blur()
    blocked.set(false)
    blocked.set(true)
    await tick()
    await Promise.resolve()
    expect(document.activeElement).toBe(document.body)
    blocked.set(false)
    await tick()
    await Promise.resolve()
    expect(document.activeElement).toBe(editor)
})
