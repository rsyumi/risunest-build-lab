import { afterEach, expect, it, vi } from 'vitest'
import { mount, unmount } from 'svelte'
import { changeLanguage, language } from 'src/lang'
import LazyScreenError from './LazyScreenError.svelte'
let component: ReturnType<typeof mount> | undefined
afterEach(async () => { if (component) await unmount(component); component = undefined; document.body.replaceChildren(); changeLanguage('en') })
it.each([
    [false, false], [false, true], [true, false], [true, true],
])('offers separate retry and close actions with restart guidance (overlay=%s, Korean=%s)', (overlay, korean) => {
    changeLanguage(korean ? 'ko' : 'en')
    const onRetry = vi.fn(), onBack = vi.fn()
    component = mount(LazyScreenError, { target: document.body, props: {
        message: language.risuNest.lazy.settings,
        backLabel: language.close, onRetry, onBack, overlay,
    } })
    const panel = document.querySelector('[role="alert"]')!
    expect(panel.textContent).toContain(language.risuNest.lazy.settings)
    expect(panel.querySelector('p')?.textContent).toBe(language.risuNest.lazy.restartHelp)
    expect(panel.classList.contains('absolute')).toBe(overlay)
    const button = (label: string) => [...panel.querySelectorAll('button')].find(item => item.textContent?.trim() === label)!
    expect(panel.querySelectorAll('button')).toHaveLength(2)
    button(language.retry).click()
    expect(onRetry).toHaveBeenCalledOnce()
    expect(onBack).not.toHaveBeenCalled()
    button(language.close).click()
    expect(onBack).toHaveBeenCalledOnce()
})
