import { afterEach, describe, expect, it, vi } from 'vitest'
import { mount, tick, unmount } from 'svelte'

const settings = vi.hoisted(() => ({ kept: [] as string[], update: vi.fn() }))
vi.mock('src/ts/storage/deviceSettings', () => ({
    getStartupExclusions: () => [...settings.kept],
    updateStartupExclusions: (value: string[]) => { settings.update(value); settings.kept = [...value] },
}))
vi.mock('src/ts/alert', () => ({ alertActionConfirm: vi.fn() }))
vi.mock('src/lang', async () => ({ language: (await import('src/lang/en')).languageEnglish }))

import Component from './RisuNestStartupExclusions.svelte'
import { languageEnglish } from 'src/lang/en'
import { languageKorean } from 'src/lang/ko'

const strings = languageEnglish.risuNest.recovery
let component: ReturnType<typeof mount> | undefined

afterEach(async () => {
    if (component) await unmount(component)
    component = undefined
    document.body.replaceChildren()
    settings.update.mockReset()
})

async function setup(kept: string[]): Promise<HTMLElement> {
    settings.kept = kept
    const target = document.createElement('div')
    document.body.append(target)
    component = mount(Component, { target })
    await tick()
    return target
}

const buttonFor = (target: HTMLElement, label: string) => [...target.querySelectorAll('button')]
    .find(button => button.closest('div.grid')?.textContent?.includes(label))!

describe('items kept off from the recovery screen', () => {
    it('shows nothing while every item is on', async () => {
        const target = await setup([])
        expect(target.textContent?.trim()).toBe('')
    })

    it('lists each item kept off by its recovery name', async () => {
        const target = await setup(['plugins', 'sync'])
        expect(target.querySelector('h2')?.textContent).toBe(strings.keptTitle)
        expect(target.textContent).toContain(strings.keptHelp)
        expect(target.textContent).toContain(strings.excludePlugins)
        expect(target.textContent).toContain(strings.excludeSync)
        expect([...target.querySelectorAll('button')].map(button => button.textContent?.trim())).toEqual([strings.turnOn, strings.turnOn])
    })

    it('turns one item back on and keeps the rest off', async () => {
        const target = await setup(['plugins', 'sync'])
        buttonFor(target, strings.excludeSync).click()
        await tick()
        expect(settings.update).toHaveBeenCalledExactlyOnceWith(['plugins'])
        expect(target.textContent).not.toContain(strings.excludeSync)
        expect(target.textContent).toContain(strings.excludePlugins)
    })

    it('removes the group once the last item is on', async () => {
        const target = await setup(['theme'])
        buttonFor(target, strings.excludeTheme).click()
        await tick()
        expect(settings.update).toHaveBeenCalledExactlyOnceWith([])
        expect(target.querySelector('h2')).toBeNull()
    })

    it('has its text in both shipped languages', () => {
        expect(languageKorean.risuNest.recovery).toMatchObject({ keptTitle: '앞으로도 끈 항목', keptHelp: '켠 항목은 앱을 다시 시작하면 적용됩니다.', turnOn: '켜기' })
        expect(languageEnglish.risuNest.recovery).toMatchObject({ keptTitle: 'Kept off', keptHelp: 'Items you turn on take effect when the app restarts.', turnOn: 'Turn on' })
    })
})
