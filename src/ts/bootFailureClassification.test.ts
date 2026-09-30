import { describe, expect, it } from 'vitest'
import { classifyBootFailure } from './bootFailureClassification'

describe('classifyBootFailure', () => {
    it('names an incompatible persistent schema wherever it is thrown', () => {
        expect(classifyBootFailure(
            new Error('unsupported persistent schema version 17'),
            'persistent-storage',
        )).toEqual({
            kind: 'schema-unsupported',
            message: 'unsupported persistent schema version 17',
            stage: 'persistent-storage',
        })
        expect(classifyBootFailure(
            new Error('unsupported persistent schema version 17'),
            'plugins',
        ).kind).toBe('schema-unsupported')
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
        expect(classifyBootFailure('unsupported persistent schema version 17')).toEqual({
            kind: 'schema-unsupported',
            message: 'unsupported persistent schema version 17',
            stage: undefined,
        })
        expect(classifyBootFailure(
            { code: 'store-error', message: 'unsupported persistent schema version 17' },
            'persistent-database',
        ).kind).toBe('schema-unsupported')
        expect(classifyBootFailure(null, 'plugins')).toEqual({
            kind: 'unknown',
            message: 'null',
            stage: 'plugins',
        })
    })
})

