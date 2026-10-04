import { afterEach, beforeEach, expect, it, vi } from 'vitest'
import { mount, tick, unmount } from 'svelte'
import { writable } from 'svelte/store'
const state = vi.hoisted(() => ({ retry: vi.fn() }))
vi.mock('src/ts/storage/persistentDataRuntime.svelte', async () => {
    const { writable } = await import('svelte/store')
    return ({
    persistentLocalSaveFailure: writable<unknown | null>(null),
    getPersistentDataRuntime: () => ({ flushPendingDataLocally: state.retry }),
}) })
vi.mock('src/lang', async () => ({ language: (await import('src/lang/en')).languageEnglish }))
import { persistentLocalSaveFailure } from 'src/ts/storage/persistentDataRuntime.svelte'
import { languageEnglish } from 'src/lang/en'
import Banner from './PersistentLocalSaveFailure.svelte'
import { PayloadTooLargeError } from 'src/ts/storage/nativePersistenceValue'
const failure = persistentLocalSaveFailure as ReturnType<typeof writable<unknown | null>>
let component: ReturnType<typeof mount>
beforeEach(() => { failure.set(null); state.retry.mockReset(); component = mount(Banner, { target: document.body }) })
afterEach(async () => { await unmount(component); document.body.replaceChildren() })
it('stays visible through pending and unsuccessful retries, clearing only with the saved state', async () => {
    failure.set(new Error('private content must not appear'))
    await tick()
    let finish!: () => void
    state.retry.mockReturnValueOnce(new Promise<void>(resolve => { finish = resolve }))
    document.querySelector('button')!.click(); await tick()
    expect(document.querySelector('[role="alert"]')).not.toBeNull()
    expect(document.querySelector('button')!.disabled).toBe(true)
    expect(state.retry).toHaveBeenCalledWith('save-failure-retry')
    finish(); await tick(); await tick()
    expect(document.querySelector('[role="alert"]')).not.toBeNull()
    state.retry.mockRejectedValueOnce(new Error('still unsaved'))
    document.querySelector('button')!.click(); await tick(); await tick()
    expect(document.querySelector('[role="alert"]')).not.toBeNull()
    expect(document.body.textContent).not.toContain('private content')
    failure.set(null); await tick()
    expect(document.querySelector('[role="alert"]')).toBeNull()
})
it('identifies the affected area without rendering values, identifiers, or raw reasons', async () => {
    failure.set({ code: 'unsaveable-value', area: 'conversation', recordId: 'private-id', reason: 'private raw value' })
    await tick()
    expect(document.body.textContent).toContain('Chat could not be saved.')
    expect(document.body.textContent).not.toContain('private')
})
it('offers storage recovery for quota failures', async () => {
    failure.set(new DOMException('private database', 'QuotaExceededError')); await tick()
    expect(document.body.textContent).toContain(languageEnglish.risuNest.localSaveFailure.storage)
    expect(document.body.textContent).not.toContain('private database')
})
it('asks to remove what was just added when a save is too large', async () => {
    failure.set(new PayloadTooLargeError('commit', 70 * 1024 * 1024)); await tick()
    expect(document.body.textContent).toContain(languageEnglish.risuNest.localSaveFailure.tooLarge)
})
