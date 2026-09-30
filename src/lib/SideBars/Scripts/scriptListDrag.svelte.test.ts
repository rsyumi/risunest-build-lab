import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { mount, tick, unmount } from 'svelte'
import { writable } from 'svelte/store'
import { languageEnglish } from 'src/lang/en'
import RegexList from './RegexList.svelte'
import TriggerList from './TriggerV1List.svelte'

const sortable = vi.hoisted(() => ({ create: vi.fn() }))
vi.mock('sortablejs', () => ({ default: { create: sortable.create } }))
vi.mock('src/lang', () => ({ language: languageEnglish }))
vi.mock('src/ts/stores.svelte', () => ({ ReloadGUIPointer: writable(0) }))
vi.mock('src/ts/util', () => ({ sortableOptions: {}, sleep: vi.fn() }))
vi.mock('src/ts/alert', () => ({ alertConfirm: vi.fn(async () => true) }))
vi.mock('src/ts/process/scripts', () => ({ exportRegex: vi.fn(), importRegex: vi.fn() }))
vi.mock('../../UI/GUI/TextAreaInput.svelte', () => ({ default: () => {} }))
vi.mock('src/lib/Others/Help.svelte', () => ({ default: () => {} }))

let instance: ReturnType<typeof mount> | undefined
let value = $state<any[]>([])
function header(name: string) {
    return [...document.querySelectorAll<HTMLButtonElement>('button')]
        .find(button => button.textContent?.trim() === name)!.parentElement!
}
async function toggle(name: string) {
    header(name).querySelector('button')!.click()
    await tick()
}
async function remove(name: string) {
    header(name).querySelectorAll('button')[1].click()
    await vi.waitFor(() => expect(value.some(item => item.comment === name)).toBe(false))
    await tick()
}
beforeEach(() => {
    sortable.create.mockReset().mockImplementation(() => ({ destroy: vi.fn() }))
    value = ['First', 'Second', 'Third'].map(comment => ({
        comment, in: '', out: '', type: 'start', conditions: [], effect: [],
    }))
})
afterEach(async () => {
    if (instance) await unmount(instance)
    instance = undefined
    document.body.replaceChildren()
})

describe.each([
    ['regex', RegexList, 'data-risu-idx'],
    ['trigger', TriggerList, 'data-risu-idx2'],
] as const)('%s dragging', (_, Component, attribute) => {
    async function setup() {
        instance = mount(Component as typeof RegexList, { target: document.body, props: {
            get value() { return value }, set value(next) { value = next },
        } })
        await tick()
    }
    it('restores dragging after deleting a closed entry and opening then closing another', async () => {
        await setup()
        await remove('First')
        await toggle('Second')
        const creations = sortable.create.mock.calls.length
        await toggle('Second')
        expect(sortable.create).toHaveBeenCalledTimes(creations + 1)
    })
    it('restores dragging when the last open entry is deleted', async () => {
        await setup()
        await toggle('First')
        const creations = sortable.create.mock.calls.length
        await remove('First')
        expect(sortable.create).toHaveBeenCalledTimes(creations + 1)
    })
    it('preserves another open editor when an earlier entry is deleted', async () => {
        await setup()
        await toggle('Second')
        const editor = header('Second').parentElement!.querySelector('input')!
        const creations = sortable.create.mock.calls.length
        await remove('First')
        expect(header('Second').parentElement!.querySelector('input')).toBe(editor)
        expect(header('Third').parentElement!.querySelector('input')).toBeNull()
        expect(sortable.create).toHaveBeenCalledTimes(creations)
    })
    it('keeps rows mounted and supports repeated reordering', async () => {
        await setup()
        const firstRow = header('First').parentElement!
        const thirdRow = header('Third').parentElement!
        for (const [item, before] of [[thirdRow, firstRow], [firstRow, thirdRow]]) {
            const [container, options] = sortable.create.mock.calls.at(-1)!
            const oldIndex = [...container.children].indexOf(item)
            options.onStart?.({ item, from: container })
            container.insertBefore(item, before)
            await options.onEnd({ item, from: container, to: container, oldIndex,
                newIndex: [...container.children].indexOf(item) })
            await tick()
        }
        expect(value.map(item => item.comment)).toEqual(['First', 'Third', 'Second'])
        expect(header('First').parentElement).toBe(firstRow)
        expect([...firstRow.parentElement!.querySelectorAll(`[${attribute}]`)]
            .map(row => row.querySelector('button')!.textContent!.trim())).toEqual(['First', 'Third', 'Second'])
        expect(sortable.create).toHaveBeenCalledTimes(1)
    })
    it('restores dragging when an open row is replaced externally', async () => {
        await setup()
        await toggle('First')
        const creations = sortable.create.mock.calls.length
        value = value.slice(1)
        await tick()
        expect(sortable.create).toHaveBeenCalledTimes(creations + 1)
        await toggle('Second')
        await toggle('Second')
        expect(sortable.create).toHaveBeenCalledTimes(creations + 2)
    })
})
