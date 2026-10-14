import { describe, expect, it } from 'vitest'

import { language } from 'src/lang'
import { backgroundErrorMessage } from './backgroundErrorMessage'

const fallback = language.risuNest.backgroundDataFailed

describe('backgroundErrorMessage', () => {
    it('keeps an Error for its message and stack', () => {
        const error = new Error('database is locked')
        expect(backgroundErrorMessage(error)).toBe(error)
    })

    it('shows the message a native store error carries', () => {
        expect(backgroundErrorMessage({ code: 'committed', revision: 3, message: 'journal write failed' })).toBe('journal write failed')
    })

    it.each([
        { code: 'commit-busy' },
        { code: 'raw-body-unavailable' },
        { code: 'transient', retryable: true },
        { kind: 'transient' },
        { code: 'committed', revision: 3, message: '' },
    ])('shows a readable sentence for a native error without a message: %o', (error) => {
        expect(backgroundErrorMessage(error)).toBe(fallback)
    })

    it('reads a native error that arrived as JSON text', () => {
        expect(backgroundErrorMessage('{"code":"commit-busy"}')).toBe(fallback)
        expect(backgroundErrorMessage('{"code":"store-error","message":"disk full"}')).toBe('disk full')
    })

    it('keeps plain text as it is', () => {
        expect(backgroundErrorMessage('offline')).toBe('offline')
        expect(backgroundErrorMessage('42')).toBe('42')
    })

    it.each([undefined, null, 7])('never shows the value itself: %o', (error) => {
        expect(backgroundErrorMessage(error)).toBe(fallback)
    })
})
