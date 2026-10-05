import { describe, expect, it } from 'vitest'
import { classifyBootFailure } from './bootFailureClassification'

describe('classifyBootFailure', () => {
    it.each([
        'unsupported persistent schema version 17',
        'invalid-lww-schema',
        'invalid-message-hash-schema',
        'Server connection schema is incompatible',
        'device store is unavailable: Device schema is incompatible',
    ])('names a store another version wrote from its native code (%s)', (message) => {
        const restored = Object.assign(new Error(message), { code: 'schema-mismatch' })
        expect(classifyBootFailure(restored, 'persistent-storage')).toEqual({
            kind: 'schema-unsupported',
            message,
            stage: 'persistent-storage',
        })
        expect(classifyBootFailure({ code: 'schema-mismatch', message }, 'device-settings').kind).toBe('schema-unsupported')
        expect(classifyBootFailure(restored, 'plugins').kind).toBe('schema-unsupported')
    })

    it('does not guess a schema mismatch from the text of another failure', () => {
        expect(classifyBootFailure(
            { code: 'store-error', message: 'unsupported persistent schema version 17' },
            'persistent-database',
        ).kind).toBe('store-open')
        expect(classifyBootFailure(new Error('invalid-lww-schema'), 'plugins').kind).toBe('unknown')
    })

    it.each(['persistent-storage', 'device-settings', 'persistent-database'] as const)(
        'treats a failure in the %s stage as a store that could not be opened',
        (stage) => {
            expect(classifyBootFailure(new Error('disk I/O error'), stage)).toEqual({
                kind: 'store-open',
                message: 'disk I/O error',
                stage,
            })
        },
    )

    it.each([undefined, 'startup', 'plugins', 'ui-state'] as const)(
        'falls back to an unknown failure for the %s stage',
        (stage) => {
            expect(classifyBootFailure(new Error('boom'), stage)).toEqual({
                kind: 'unknown',
                message: 'boom',
                stage,
            })
        },
    )

    it('reads a message out of a raw string and a native rejection object', () => {
        expect(classifyBootFailure('disk I/O error')).toEqual({
            kind: 'unknown',
            message: 'disk I/O error',
            stage: undefined,
        })
        expect(classifyBootFailure({ code: 'store-error', message: 'disk I/O error' }, 'persistent-database')).toEqual({
            kind: 'store-open',
            message: 'disk I/O error',
            stage: 'persistent-database',
        })
        expect(classifyBootFailure(null, 'plugins')).toEqual({
            kind: 'unknown',
            message: 'null',
            stage: 'plugins',
        })
    })
})

