import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { mount, unmount } from 'svelte'
import SettingProgress from './SettingProgress.svelte'

vi.mock('@lucide/svelte', () => {
    const icon = (name: string) => (anchor: ChildNode) => {
        const marker = document.createElement('i')
        marker.dataset.icon = name
        anchor.before(marker)
    }
    return { CheckIcon: icon('check'), LoaderCircleIcon: icon('spinner') }
})

let target: HTMLDivElement
let mounted: ReturnType<typeof mount> | undefined

beforeEach(() => {
    target = document.createElement('div')
    document.body.append(target)
})

afterEach(async () => {
    try {
        if (mounted) await unmount(mounted)
    } finally {
        mounted = undefined
        target.remove()
    }
})

const icons = () => [...target.querySelectorAll<HTMLElement>('[data-icon]')].map(icon => icon.dataset.icon)
const pulsing = () => target.querySelector('[role="progressbar"] .motion-safe\\:animate-pulse') !== null

describe('SettingProgress', () => {
    it('spins and pulses while the operation runs without a total', () => {
        mounted = mount(SettingProgress, { target, props: { label: 'Synthetic export' } })
        expect(icons()).toEqual(['spinner'])
        expect(pulsing()).toBe(true)
    })

    it('shows a check once the operation finished', () => {
        mounted = mount(SettingProgress, { target, props: { label: 'Synthetic export', fraction: 1, done: true } })
        expect(icons()).toEqual(['check'])
        expect(target.querySelector('[role="progressbar"]')?.getAttribute('aria-valuenow')).toBe('100')
    })

    it('stops the spinner and the pulse when the operation ended without finishing', () => {
        mounted = mount(SettingProgress, { target, props: { label: 'Synthetic export', stopped: true } })
        expect(icons()).toEqual([])
        expect(pulsing()).toBe(false)
    })
})
