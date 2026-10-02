// @vitest-environment happy-dom

import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { mount, tick, unmount } from 'svelte'
import { get } from 'svelte/store'
import { alertStore } from 'src/ts/stores.svelte'

const branchMocks = vi.hoisted(() => ({
    getChatBranches: vi.fn(),
}))

vi.mock('src/ts/gui/branches', () => branchMocks)
vi.mock('src/ts/stores.svelte', async () => {
    const { writable } = await import('svelte/store')
    return {
        DBState: { db: { characters: [], botPresets: [], botPresetsId: 0 } },
        alertStore: writable({ type: 'none', msg: '' }),
        selectedCharID: writable(-1),
    }
})
vi.mock('src/ts/util', () => ({ sleep: vi.fn() }))
vi.mock('src/ts/storage/database.svelte', () => ({ getDatabase: () => ({}) }))
vi.mock('src/ts/storage/deviceMarkers', () => ({ getDeviceMarkers: vi.fn() }))
vi.mock('../../ts/characters', () => ({ getCharImage: vi.fn() }))
vi.mock('../../ts/parser/parser.svelte', () => ({ ParseMarkdown: vi.fn() }))
vi.mock('src/ts/characterCards', () => ({
    hubURL: '',
    isCharacterHasAssets: () => false,
}))
vi.mock('src/ts/globalApi.svelte', () => ({
    aiLawApplies: () => false,
    getFetchData: vi.fn(),
    getFetchLogs: () => [],
    openURL: vi.fn(),
}))
vi.mock('src/ts/tokenizer', () => ({
    tokenize: vi.fn(async (text: string) => `tokens:${text}`),
}))
vi.mock('src/ts/gui/colorscheme', async () => {
    const { writable } = await import('svelte/store')
    return { ColorSchemeTypeStore: writable(false) }
})
vi.mock('../../ts/sourcemap', () => ({ translateStackTrace: vi.fn() }))
vi.mock('src/ts/platform', () => ({
    isTauri: false,
    getDetailedOSLabel: async () => 'Test OS',
    getFallbackOSLabel: () => 'Test OS',
    getRisuEnvironmentLabel: () => 'Test',
}))
vi.mock('src/ts/storage/officialAccountMessage', () => ({
    isExpectedHubMessage: () => false,
}))
vi.mock('@lucide/svelte', async () => {
    const { default: Stub } = await import('./AlertCompDependencyStub.test.svelte')
    return {
        CheckIcon: Stub,
        ChevronDownIcon: Stub,
        ChevronRightIcon: Stub,
        ChevronUpIcon: Stub,
        CopyIcon: Stub,
        User: Stub,
        XIcon: Stub,
    }
})
vi.mock('../SideBars/BarIcon.svelte', async () => ({
    default: (await import('./AlertCompDependencyStub.test.svelte')).default,
}))
vi.mock('../UI/GUI/TextInput.svelte', async () => ({
    default: (await import('./AlertCompDependencyStub.test.svelte')).default,
}))
vi.mock('../UI/GUI/Button.svelte', async () => ({
    default: (await import('./AlertCompButtonStub.test.svelte')).default,
}))
vi.mock('../UI/GUI/SelectInput.svelte', async () => ({
    default: (await import('./AlertCompDependencyStub.test.svelte')).default,
}))
vi.mock('../UI/GUI/OptionInput.svelte', async () => ({
    default: (await import('./AlertCompDependencyStub.test.svelte')).default,
}))
vi.mock('../UI/DeferredMarkdown.svelte', async () => ({
    default: (await import('./AlertCompDependencyStub.test.svelte')).default,
}))
vi.mock('../UI/GUI/TextAreaInput.svelte', async () => ({
    default: (await import('./AlertCompDependencyStub.test.svelte')).default,
}))
vi.mock('../Setting/Pages/Module/ModuleChatMenu.svelte', async () => ({
    default: (await import('./AlertCompDependencyStub.test.svelte')).default,
}))
vi.mock('./Help.svelte', async () => ({
    default: (await import('./AlertCompDependencyStub.test.svelte')).default,
}))

import { alertCheckboxConfirm } from 'src/ts/alert'
import AlertComp from './AlertComp.svelte'

let mounted: ReturnType<typeof mount> | undefined
let target: HTMLDivElement
const options = { title: 'Replace data?', description: 'Current data is cleared.', checkboxLabel: 'Clear current data', actionLabel: 'Replace', cancelLabel: 'Cancel', requireChecked: true }
afterEach(async () => {
    alertStore.set({ type: 'none', msg: '' })
    if (mounted) await unmount(mounted)
    mounted = undefined
    document.body.replaceChildren()
})
async function show(requireChecked = true) {
    const result = alertCheckboxConfirm({ ...options, requireChecked })
    target = document.createElement('div')
    document.body.append(target)
    mounted = mount(AlertComp, { target })
    await tick()
    return { result }
}
function action(label: string) {
    return [...target.querySelectorAll('button')].find(button => button.textContent?.trim() === label)!
}
describe('checkbox confirmations', () => {
    it('gates acknowledgement until checked and reports the selection', async () => {
        const { result } = await show()
        expect(target.textContent).toContain(options.description)
        expect(action('Replace').disabled).toBe(true)
        const checkbox = target.querySelector<HTMLInputElement>('input')!
        expect(checkbox.checked).toBe(false)
        checkbox.click()
        await tick()
        expect(action('Replace').disabled).toBe(false)
        action('Replace').click()
        await expect(result).resolves.toEqual({ confirmed: true, checked: true })
    })
    it.each([false, true])('allows optional action with selection %s', async checked => {
        const { result } = await show(false)
        expect(action('Replace').disabled).toBe(false)
        if (checked) { target.querySelector<HTMLInputElement>('input')!.click(); await tick() }
        action('Replace').click()
        await expect(result).resolves.toEqual({ confirmed: true, checked })
    })
    it.each(['button', 'Escape', 'overlay'])('cancels by %s without confirmation', async mode => {
        const { result } = await show()
        if (mode === 'button') action('Cancel').click()
        else if (mode === 'Escape') window.dispatchEvent(new KeyboardEvent('keydown', { key: 'Escape' }))
        else target.querySelector<HTMLButtonElement>('button[aria-label="Cancel"]')!.click()
        await expect(result).resolves.toEqual({ confirmed: false, checked: false })
    })
    it('resolves cancellation if another alert replaces the dialog', async () => {
        const { result } = await show()
        alertStore.set({ type: 'none', msg: '' })
        await expect(result).resolves.toEqual({ confirmed: false, checked: false })
    })
    it('starts a consecutive dialog unchecked', async () => {
        const { result } = await show(false)
        target.querySelector<HTMLInputElement>('input')!.click()
        await tick()
        action('Replace').click()
        await result
        const next = alertCheckboxConfirm({ ...options })
        await tick()
        expect(target.querySelector<HTMLInputElement>('input')!.checked).toBe(false)
        expect(action('Replace').disabled).toBe(true)
        action('Cancel').click()
        await next
    })
})
