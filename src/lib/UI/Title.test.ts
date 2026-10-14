import { afterEach, expect, it, vi } from 'vitest'
import { flushSync, mount, unmount } from 'svelte'
import type { SpecialDay } from 'src/ts/ui/specialDay'

const state = vi.hoisted(() => ({ day: null as SpecialDay, years: 0, scheme: null as unknown as import('svelte/store').Writable<'dark' | 'light'> }))
vi.mock('src/ts/gui/colorscheme', async () => {
    const { writable } = await import('svelte/store')
    state.scheme = writable<'dark' | 'light'>('dark')
    return { ColorSchemeTypeStore: state.scheme }
})
vi.mock('src/ts/stores.svelte', () => ({ DBState: { db: { language: 'en' } } }))
vi.mock('src/ts/globalApi.svelte', () => ({ openURL: vi.fn() }))
vi.mock('src/ts/ui/specialDay', async (original) => ({
    ...(await original<typeof import('src/ts/ui/specialDay')>()),
    getSpecialDay: () => state.day,
    anniversaryYears: () => state.years,
}))
import Title from './Title.svelte'

let component: ReturnType<typeof mount> | undefined
afterEach(async () => { if (component) await unmount(component); component = undefined; document.body.replaceChildren(); state.scheme.set('dark') })

function render(day: SpecialDay, years = 0) {
    if (component) { unmount(component); document.body.replaceChildren() }
    state.day = day
    state.years = years
    component = mount(Title, { target: document.body })
    return document.querySelector('h2')!
}

it('shows the nest mark, the title and the byline on an ordinary day', () => {
    const title = render(null)
    expect(title.textContent?.replace(/\s+/g, ' ').trim()).toBe('RisuNest by. Yumi')
    const mark = title.querySelector('svg')!
    expect(mark.querySelectorAll('path')).toHaveLength(2)
    expect(mark.querySelector('path')?.getAttribute('fill')).toBe('url(#risunest-title-lip)')
    expect(mark.classList.contains('rotate-180')).toBe(false)
    expect(document.querySelector('h1')).toBeNull()
})

it('draws the mark in the text color on light themes', () => {
    state.scheme.set('light')
    const mark = render(null).querySelector('svg')!
    expect([...mark.querySelectorAll('path')].map((path) => path.getAttribute('fill'))).toEqual(['currentColor', 'currentColor'])
})

it('keeps the title text plain and decorates only the mark on special days', () => {
    const plain = render(null).querySelector('svg')!.innerHTML
    for (const day of ['christmas', 'newYear', 'halloween', 'harvestMoon', 'anniversary'] as const) {
        const title = render(day, day === 'anniversary' ? 2 : 0)
        expect(title.textContent?.replace(/\s+/g, ' ').trim()).toBe('RisuNest by. Yumi')
        expect(title.querySelector('svg')!.innerHTML).not.toBe(plain)
        expect(title.querySelector('svg')!.querySelectorAll('path, circle, rect, ellipse').length).toBeGreaterThan(2)
    }
    expect(render('aprilFool').querySelector('svg')!.classList.contains('rotate-180')).toBe(true)
})

it('counts anniversaries from the release date', () => {
    render('anniversary', 2)
    expect(document.querySelector('h1')?.textContent?.replace(/\s+/g, ' ').trim()).toBe('Happy 2nd Anniversary!')
})

it('opens the mini game after five clicks on the santa hat', () => {
    const title = render('christmas')
    const hat = title.querySelector<SVGGElement>('svg g.cursor-pointer')!
    for (let i = 0; i < 5; i++) hat.dispatchEvent(new MouseEvent('click', { bubbles: true }))
    flushSync()
    expect(document.getElementById('minigame-div')).not.toBeNull()
    expect(title.querySelector('svg g.cursor-pointer')).toBeNull()
})
