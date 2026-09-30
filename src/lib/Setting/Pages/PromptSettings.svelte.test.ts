import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { mount, tick, unmount } from 'svelte'
import { languageEnglish } from 'src/lang/en'
import { DBState } from 'src/ts/stores.svelte'
import { RISU_PROMPT_DRAG_TYPE } from 'src/ts/dragTypes'
import PromptSettings from './PromptSettings.svelte'

vi.mock('src/lang', () => ({ language: languageEnglish }))
vi.mock('src/ts/stores.svelte', () => {
    const DBState = $state({ db: {} })
    return { DBState }
})
vi.mock('src/ts/process/prompt', () => ({ tokenizePreset: vi.fn(async () => 0) }))
vi.mock('src/ts/process/templates/templateCheck', () => ({ templateCheck: () => [] }))
vi.mock('src/lib/Others/Help.svelte', () => ({ default: () => {} }))
vi.mock('src/lib/UI/GUI/TextAreaInput.svelte', () => ({ default: () => {} }))
vi.mock('src/lib/UI/ModelList.svelte', () => ({ default: () => {} }))
vi.mock('./Model/AuxModelSelectors.svelte', () => ({ default: () => {} }))

let instance: ReturnType<typeof mount>
function header(name: string) {
    return [...document.querySelectorAll<HTMLElement>('[draggable="true"]')]
        .find(element => element.querySelector('span')?.textContent === name)!
}
function transfer() {
    const data = new Map<string, string>()
    return { get types() { return [...data.keys()] }, setData: (key: string, value: string) => data.set(key, value),
        getData: (key: string) => data.get(key) ?? '', setDragImage: vi.fn() }
}
async function dragEvent(element: Element, type: string, dataTransfer: ReturnType<typeof transfer>, clientY = 0) {
    const event = new Event(type, { bubbles: true, cancelable: true })
    Object.assign(event, { dataTransfer, clientY })
    element.dispatchEvent(event)
    await tick()
}
beforeEach(async () => {
    DBState.db = { promptTemplate: ['First', 'Second', 'Last'].map(name => ({
        name, type: 'plain', text: '', role: 'system', type2: 'normal',
    })), promptSettings: {} } as any
    instance = mount(PromptSettings, { target: document.body })
    await tick()
})
afterEach(async () => {
    await unmount(instance)
    document.body.replaceChildren()
})

describe('prompt drag ordering', () => {
    it('keeps the same prompt open when another operation inserts an earlier item', async () => {
        header('Second').click()
        await tick()
        const input = header('Second').parentElement!.querySelector('input')!
        DBState.db.promptTemplate.unshift({ type: 'plain', name: 'Inserted', text: '', role: 'system', type2: 'normal' })
        await tick()
        expect(header('Second').parentElement!.querySelector('input')).toBe(input)
        expect(header('First').parentElement!.querySelector('input')).toBeNull()
    })
    it('drops before the gap being hovered, independent of the previous card hover', async () => {
        const data = transfer()
        await dragEvent(header('Last'), 'dragstart', data)
        const gap = header('First').parentElement!.previousElementSibling!
        await dragEvent(gap, 'dragover', data)
        await dragEvent(gap, 'drop', data)
        expect(DBState.db.promptTemplate.map(item => item.name)).toEqual(['Last', 'First', 'Second'])
    })
    it('keeps the source and open editors stable during hover, drop, and subsequent editing', async () => {
        const first = header('First')
        first.click()
        await tick()
        const input = first.parentElement!.querySelector('input')!
        const data = transfer()
        await dragEvent(first, 'dragstart', data)
        await dragEvent(header('Last').parentElement!, 'dragover', data, 20)
        expect(header('First')).toBe(first)
        expect(first.parentElement!.querySelector('input')).toBe(input)
        expect(DBState.db.promptTemplate.map(item => item.name)).toEqual(['First', 'Second', 'Last'])
        await dragEvent(header('Last').parentElement!, 'drop', data, 20)
        expect(DBState.db.promptTemplate.map(item => item.name)).toEqual(['Second', 'Last', 'First'])
        expect(header('First')).toBe(first)
        expect(first.parentElement!.querySelector('input')).toBe(input)
        input.value = 'Edited'
        input.dispatchEvent(new Event('input', { bubbles: true }))
        await tick()
        expect(DBState.db.promptTemplate.map(item => item.name)).toEqual(['Second', 'Last', 'Edited'])
    })
    it('ignores unrelated drags and resets a cancelled prompt drag', async () => {
        const data = transfer()
        data.setData('text/plain', 'unrelated')
        await dragEvent(header('First').parentElement!, 'drop', data)
        expect(data.types).not.toContain(RISU_PROMPT_DRAG_TYPE)
        await dragEvent(header('Last'), 'dragstart', data)
        await dragEvent(header('First').parentElement!, 'dragover', data)
        await dragEvent(header('Last'), 'dragend', data)
        expect(DBState.db.promptTemplate.map(item => item.name)).toEqual(['First', 'Second', 'Last'])
        expect(document.querySelector('.opacity-50')).toBeNull()
    })
    it('does not reuse a previous target when the prompt is dropped back on itself', async () => {
        const data = transfer()
        await dragEvent(header('First'), 'dragstart', data)
        await dragEvent(header('Last').parentElement!, 'dragover', data, 20)
        await dragEvent(header('First').parentElement!, 'dragover', data, 20)
        await dragEvent(header('First').parentElement!, 'drop', data, 20)
        expect(DBState.db.promptTemplate.map(item => item.name)).toEqual(['First', 'Second', 'Last'])
    })
})
