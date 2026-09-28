import { afterEach, expect, it } from 'vitest'
import { flushSync, mount, unmount } from 'svelte'
import SwitchInput from './SwitchInput.svelte'

let instance: ReturnType<typeof mount> | undefined

function render(props: { check: boolean; disabled?: boolean }) {
    instance = mount(SwitchInput, { target: document.body, props: { name: 'Switch', ...props } })
    flushSync()
    const input = document.querySelector<HTMLInputElement>('input[role="switch"]')!
    const track = input.nextElementSibling as HTMLElement
    const thumb = track.firstElementChild as HTMLElement
    return { input, track, thumb }
}

afterEach(() => {
    if (instance) unmount(instance)
    instance = undefined
    document.body.replaceChildren()
})

it('draws the on thumb in the primary foreground role on the primary track', () => {
    const { track, thumb } = render({ check: true })
    expect(track.classList).toContain('bg-primary-500')
    expect(thumb.classList).toContain('bg-primary-foreground')
    expect(thumb.classList).not.toContain('bg-white')
})

it('draws the off thumb in the text role on the neutral track', () => {
    const { track, thumb } = render({ check: false })
    expect(track.classList).toContain('bg-darkbutton')
    expect(thumb.classList).toContain('bg-textcolor')
    expect(thumb.classList).not.toContain('bg-primary-foreground')
    expect(thumb.classList).not.toContain('bg-white')
})

it('switches the thumb role when the user toggles it', () => {
    const { input, thumb } = render({ check: false })
    input.click()
    flushSync()
    expect(input.checked).toBe(true)
    expect(thumb.classList).toContain('bg-primary-foreground')
    input.click()
    flushSync()
    expect(thumb.classList).toContain('bg-textcolor')
})

it('keeps the thumb roles while disabled and dims the track instead', () => {
    for (const check of [true, false]) {
        const { input, track, thumb } = render({ check, disabled: true })
        expect(input.disabled).toBe(true)
        expect(track.classList).toContain('peer-disabled:opacity-50')
        expect(thumb.classList).toContain(check ? 'bg-primary-foreground' : 'bg-textcolor')
        unmount(instance!)
        instance = undefined
    }
})

it('shows keyboard focus with a theme ring around the track', () => {
    const { track } = render({ check: true })
    expect(track.classList).toContain('peer-focus-visible:ring-2')
    expect(track.classList).toContain('peer-focus-visible:ring-borderc')
    expect(track.classList).toContain('peer-focus-visible:ring-offset-bgcolor')
})
