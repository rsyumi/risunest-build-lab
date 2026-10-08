// @vitest-environment happy-dom
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { get } from 'svelte/store'

vi.mock('./storage/deviceMarkers', () => ({
    getDeviceMarkers: () => ({ getItem: () => null, setItem: () => {}, flush: async () => {} }),
}))
vi.mock('./stores.svelte', async () => {
    const { createAlertQueue } = await import('./alertQueue')
    return { alertStore: createAlertQueue({ type: 'none', msg: '' }, { gapMs: 0 }) }
})
vi.mock('./storage/database.svelte', () => ({ getDatabase: vi.fn(() => ({})) }))
vi.mock('src/ts/platform', () => ({ isTauri: false }))
vi.mock('src/lang', async () => ({ language: (await import('src/lang/en')).languageEnglish }))

import { alertStore } from './stores.svelte'
import { alertClear, alertError } from './alert'
import { languageEnglish } from 'src/lang/en'
import {
    WindowedConversationRequiresCompatibilityError,
    WindowedConversationSaveError,
} from './storage/saveCoordinator'

beforeEach(() => {
    vi.useFakeTimers()
    vi.spyOn(console, 'error').mockImplementation(() => {})
})
afterEach(async () => {
    while (alertStore.hasDialogs()) {
        alertStore.set({ type: 'none', msg: '' })
        await vi.advanceTimersByTimeAsync(0)
    }
    alertClear()
    vi.useRealTimers()
    vi.restoreAllMocks()
})

describe('error dialogs for local save failures', () => {
    it.each([
        [
            'a refused windowed capture',
            new WindowedConversationRequiresCompatibilityError('private reason'),
            languageEnglish.risuNest.localSaveFailure.failed,
        ],
        [
            'a current chat that cannot be completed',
            new WindowedConversationSaveError('private reason', new Error('private cause')),
            languageEnglish.risuNest.localSaveFailure.currentChat,
        ],
    ])('shows the save failure text for %s instead of the internal error', (_label, error, text) => {
        alertError(error)
        const shown = get(alertStore)
        expect(shown).toMatchObject({ type: 'error', msg: text })
        expect(shown.stackTrace).toBeUndefined()
        expect(JSON.stringify(shown)).not.toContain('private')
        expect(JSON.stringify(shown)).not.toContain('Windowed')
    })

    it('keeps the message of other errors', () => {
        alertError(new Error('Synthetic import failure'))
        expect(get(alertStore)).toMatchObject({ type: 'error', msg: 'Synthetic import failure' })
    })
})
