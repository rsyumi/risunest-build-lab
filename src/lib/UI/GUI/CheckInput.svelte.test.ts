import { afterEach, expect, it, vi } from 'vitest'
import { mount, tick, unmount } from 'svelte'
import CheckInput from './CheckInput.svelte'

let instance: ReturnType<typeof mount> | undefined
afterEach(async () => {
    if (instance) await unmount(instance)
    instance = undefined
    document.body.replaceChildren()
})

it('reports the new value when enabling and disabling a controlled option', async () => {
    const state = $state({ check: false })
    const onChange = vi.fn((next: boolean) => { state.check = next })
    instance = mount(CheckInput, { target: document.body, props: {
        get check() { return state.check }, onChange, name: 'Synthetic option',
    } })
    const input = document.querySelector<HTMLInputElement>('input')!
    input.click()
    await tick()
    expect(onChange).toHaveBeenLastCalledWith(true)
    expect(state.check).toBe(true)
    expect(input.checked).toBe(true)
    input.click()
    await tick()
    expect(onChange).toHaveBeenLastCalledWith(false)
    expect(state.check).toBe(false)
    expect(input.checked).toBe(false)
})
