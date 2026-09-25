// @vitest-environment happy-dom

import { afterEach, describe, expect, it, vi } from 'vitest'
import { flushSync, mount, tick, unmount } from 'svelte'
import TextInput from './TextInput.svelte'
import BoundTextInput from './TextInput.test.svelte'

const mounted: ReturnType<typeof mount>[] = []

function render(component: Parameters<typeof mount>[0], props: Record<string, unknown>) {
    const target = document.createElement('div')
    document.body.append(target)
    const instance = mount(component, { target, props })
    mounted.push(instance)
    flushSync()
    return { target, instance, input: target.querySelector('input')! }
}

afterEach(async () => {
    for (const instance of mounted.splice(0)) await unmount(instance)
    document.body.replaceChildren()
})

describe('TextInput native attributes', () => {
    it.each([false, true])('renders the accessibility and length attributes when hideText is %s', (hideText) => {
        const { input } = render(TextInput, {
            value: '',
            hideText,
            ariaLabel: 'Search keys',
            ariaDescribedby: 'search-help',
            ariaInvalid: true,
            maxlength: 12,
            autocapitalize: 'none',
            spellcheck: false,
        })

        expect(input.type).toBe(hideText ? 'password' : 'text')
        expect(input.getAttribute('aria-label')).toBe('Search keys')
        expect(input.getAttribute('aria-describedby')).toBe('search-help')
        expect(input.getAttribute('aria-invalid')).toBe('true')
        expect(input.getAttribute('maxlength')).toBe('12')
        expect(input.getAttribute('autocapitalize')).toBe('none')
        expect(input.getAttribute('spellcheck')).toBe('false')
    })

    it.each([false, true])('omits the optional attributes that were not given when hideText is %s', (hideText) => {
        const { input } = render(TextInput, { value: '', hideText })

        for (const name of ['aria-label', 'aria-describedby', 'aria-invalid', 'maxlength', 'autocapitalize', 'spellcheck']) {
            expect(input.hasAttribute(name), name).toBe(false)
        }
    })

    it('keeps new-password for a hidden input and off for a plain input by default', () => {
        expect(render(TextInput, { value: '', hideText: true }).input.getAttribute('autocomplete')).toBe('new-password')
        expect(render(TextInput, { value: '' }).input.getAttribute('autocomplete')).toBe('off')
    })

    it('uses an explicit autocomplete value in either branch', () => {
        expect(render(TextInput, { value: '', hideText: true, autocomplete: 'one-time-code' }).input.getAttribute('autocomplete')).toBe('one-time-code')
        expect(render(TextInput, { value: '', hideText: true, autocomplete: 'current-password' }).input.getAttribute('autocomplete')).toBe('current-password')
        expect(render(TextInput, { value: '', autocomplete: 'username' }).input.getAttribute('autocomplete')).toBe('username')
        expect(render(TextInput, { value: '', autocomplete: 'on' }).input.getAttribute('autocomplete')).toBe('on')
    })

    it('keeps type and value under the caller props', () => {
        const { input } = render(TextInput, { value: 'kept', hideText: true, maxlength: 4 })

        expect(input.type).toBe('password')
        expect(input.value).toBe('kept')
    })
})

describe('TextInput binding', () => {
    it.each([false, true])('delivers one Korean and emoji input event to the binding and callback when hideText is %s', async (hideText) => {
        const oninput = vi.fn()
        const onchange = vi.fn()
        const { target, instance, input } = render(BoundTextInput, { hideText, oninput, onchange })
        const typed = '안녕하세요 👋🏽 테스트'

        input.value = typed
        input.dispatchEvent(new Event('input', { bubbles: true }))
        await tick()

        expect(oninput).toHaveBeenCalledTimes(1)
        expect((instance as { read(): string }).read()).toBe(typed)
        expect(target.querySelector('[data-bound]')!.textContent).toBe(typed)

        input.dispatchEvent(new Event('change', { bubbles: true }))
        await tick()

        expect(onchange).toHaveBeenCalledTimes(1)
        expect(oninput).toHaveBeenCalledTimes(1)
    })

    it.each([false, true])('writes a parent value back to the input when hideText is %s', async (hideText) => {
        const { instance, input } = render(BoundTextInput, { hideText, initial: '처음' })

        expect(input.value).toBe('처음')
        ;(instance as { write(next: string): void }).write('다음 🙂')
        await tick()

        expect(input.value).toBe('다음 🙂')
    })
})
