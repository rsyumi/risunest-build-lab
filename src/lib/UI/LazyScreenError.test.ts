import { afterEach, expect, it, vi } from 'vitest'
import { mount, unmount } from 'svelte'
const locale = vi.hoisted(() => ({ korean: false }))
vi.mock('src/lang', async () => {
    const en = (await import('src/lang/en')).languageEnglish
    const ko = (await import('src/lang/ko')).languageKorean
    return { get language() { return locale.korean ? ko : en } }
})
import { languageEnglish } from 'src/lang/en'
import { languageKorean } from 'src/lang/ko'
import LazyScreenError from './LazyScreenError.svelte'
let component: ReturnType<typeof mount> | undefined
afterEach(async () => { if (component) await unmount(component); component = undefined; document.body.replaceChildren() })
it.each([
    [false, false], [false, true], [true, false], [true, true],
])('offers separate retry and close actions with restart guidance (overlay=%s, Korean=%s)', (overlay, korean) => {
    locale.korean = korean
    const language = korean ? languageKorean : languageEnglish
    const onRetry = vi.fn(), onBack = vi.fn()
    component = mount(LazyScreenError, { target: document.body, props: {
        message: language.risuNest.lazy.settings,
        backLabel: language.close, onRetry, onBack, overlay,
    } })
    const panel = document.querySelector('[role="alert"]')!
    expect(panel.textContent).toContain(language.risuNest.lazy.settings)
    expect(panel.querySelector('p')?.textContent).toBe(korean
        ? '계속 실패할 경우 앱을 다시 실행해주세요.'
        : 'If loading still fails, restart the app.')
    expect(panel.classList.contains('absolute')).toBe(overlay)
    const button = (label: string) => [...panel.querySelectorAll('button')].find(item => item.textContent?.trim() === label)!
    expect(panel.querySelectorAll('button')).toHaveLength(2)
    button(language.retry).click()
    expect(onRetry).toHaveBeenCalledOnce()
    expect(onBack).not.toHaveBeenCalled()
    button(language.close).click()
    expect(onBack).toHaveBeenCalledOnce()
})
