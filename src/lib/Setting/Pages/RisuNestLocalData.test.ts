// @vitest-environment happy-dom

import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { mount, tick, unmount } from 'svelte'

const sections = vi.hoisted(() => ({
    readLocalDataParticipation: vi.fn(),
    setLocalDataParticipating: vi.fn(),
}))
const remotes = vi.hoisted(() => ({ readLocalDataRemoteState: vi.fn() }))

vi.mock('src/ts/storage/localDataSections', () => sections)
vi.mock('src/ts/storage/localDataRemotes', () => remotes)
vi.mock('src/ts/platform', () => ({ isTauri: false }))
vi.mock('src/ts/alert', () => ({ alertConfirm: vi.fn(), alertError: vi.fn() }))
vi.mock('src/lang', async () => ({
    language: (await import('src/lang/en')).languageEnglish,
}))

import RisuNestLocalData from './RisuNestLocalData.svelte'
import { languageEnglish } from 'src/lang/en'

const strings = languageEnglish.risuNest.localData

let target: HTMLDivElement
let component: ReturnType<typeof mount> | undefined

function toggle(section: 'hypa' | 'local-plugins'): HTMLInputElement {
    const input = target.querySelector<HTMLInputElement>(`#local-data-${section}`)
    if (!input) throw new Error(`toggle ${section} is not rendered`)
    return input
}

function dialog(): HTMLElement | null {
    return target.querySelector<HTMLElement>('[data-local-data-enable]')
}

function dialogButton(label: string): HTMLButtonElement {
    const open = dialog()
    if (!open) throw new Error('the confirmation is not open')
    const button = [...open.querySelectorAll('button')].find(
        (candidate) => candidate.textContent?.trim() === label,
    )
    if (!button) throw new Error(`button ${label} is not rendered`)
    return button
}

async function settle(): Promise<void> {
    await Promise.resolve()
    await tick()
    await Promise.resolve()
    await tick()
}

beforeEach(async () => {
    vi.clearAllMocks()
    sections.readLocalDataParticipation.mockResolvedValue([
        { section: 'hypa', participating: true },
        { section: 'local-plugins', participating: false },
    ])
    sections.setLocalDataParticipating.mockResolvedValue(undefined)
    remotes.readLocalDataRemoteState.mockResolvedValue('connected')
    target = document.createElement('div')
    document.body.append(target)
})

afterEach(() => {
    if (component) unmount(component)
    component = undefined
    target.remove()
})

describe('RisuNestLocalData', () => {
    it('shows the stored participation as the starting state', async () => {
        component = mount(RisuNestLocalData, { target })
        await settle()
        expect(target.querySelector('#local-data-hypa')).toBeNull()
        expect(toggle('local-plugins').checked).toBe(false)
        expect(dialog()).toBeNull()
    })

    it('turning a section on confirms first, then sets section participating', async () => {
        component = mount(RisuNestLocalData, { target })
        await settle()

        toggle('local-plugins').click()
        await settle()
        expect(sections.setLocalDataParticipating).not.toHaveBeenCalled()
        expect(dialog()?.textContent).toContain(strings.enableTitle)
        expect(dialog()?.textContent).toContain(strings.enableBodyPlugin)

        dialogButton(strings.enableConfirm).click()
        await settle()
        expect(sections.setLocalDataParticipating).toHaveBeenCalledWith('local-plugins', true)
        expect(dialog()).toBeNull()
        expect(toggle('local-plugins').checked).toBe(true)
    })

    it('cancelling the confirmation leaves the section off and writes nothing', async () => {
        component = mount(RisuNestLocalData, { target })
        await settle()

        toggle('local-plugins').click()
        await settle()
        dialogButton(languageEnglish.cancel).click()
        await settle()

        expect(sections.setLocalDataParticipating).not.toHaveBeenCalled()
        expect(dialog()).toBeNull()
        expect(toggle('local-plugins').checked).toBe(false)
    })

    it('turning plugin-local participation off writes without confirming', async () => {
        sections.readLocalDataParticipation.mockResolvedValue([{ section: 'hypa', participating: true }, { section: 'local-plugins', participating: true }])
        component = mount(RisuNestLocalData, { target })
        await settle()

        toggle('local-plugins').click()
        await settle()

        expect(dialog()).toBeNull()
        expect(sections.setLocalDataParticipating).toHaveBeenCalledWith('local-plugins', false)
        expect(toggle('local-plugins').checked).toBe(false)
    })

    it('offers the plugin note for the single plugin-local opt-in', async () => {
        sections.readLocalDataParticipation.mockResolvedValue([
            { section: 'hypa', participating: false },
            { section: 'local-plugins', participating: false },
        ])
        component = mount(RisuNestLocalData, { target })
        await settle()

        toggle('local-plugins').click()
        await settle()
        expect(dialog()?.textContent).toContain(strings.enableBodyPlugin)
        expect(target.querySelector('#local-data-hypa')).toBeNull()
    })

    it('says nothing is connected only once both remotes answered with none', async () => {
        component = mount(RisuNestLocalData, { target })
        await settle()
        expect(target.textContent).not.toContain(strings.notConnected)
        unmount(component)

        remotes.readLocalDataRemoteState.mockResolvedValue('unknown')
        component = mount(RisuNestLocalData, { target })
        await settle()
        expect(target.textContent).not.toContain(strings.notConnected)
        unmount(component)

        remotes.readLocalDataRemoteState.mockResolvedValue('none')
        component = mount(RisuNestLocalData, { target })
        await settle()
        expect(target.textContent).toContain(strings.notConnected)
    })

    it('shows what the store holds when the write fails', async () => {
        component = mount(RisuNestLocalData, { target })
        await settle()

        sections.setLocalDataParticipating.mockRejectedValueOnce(new Error('store is closed'))
        toggle('local-plugins').click()
        await settle()
        dialogButton(strings.enableConfirm).click()
        await settle()

        expect(toggle('local-plugins').checked).toBe(false)
    })
})

it('restores confirmed values after a write and reload fail, then retries', async () => {
    component = mount(RisuNestLocalData, { target })
    await settle()
    sections.setLocalDataParticipating.mockRejectedValueOnce(new Error('synthetic write'))
    sections.readLocalDataParticipation.mockRejectedValueOnce(new Error('synthetic reload'))
    toggle('local-plugins').click()
    await settle()
    dialogButton(strings.enableConfirm).click()
    await settle()
    expect(toggle('local-plugins').checked).toBe(false)
    expect(toggle('local-plugins').disabled).toBe(true)
    expect(target.textContent).toContain(strings.applyFailed)
    ;[...target.querySelectorAll('button')].find(button => button.textContent?.trim() === languageEnglish.retry)!.click()
    await settle()
    expect(toggle('local-plugins').disabled).toBe(false)
})
