import { describe, expect, it } from 'vitest'
import { externalErrorMessage, externalFolderErrorKind, externalStorageStrings } from './strings'

const strings = externalStorageStrings('en')

describe('externalErrorMessage', () => {
    it('reads the kind of a rejected command and the code of a job error', () => {
        expect(externalErrorMessage(strings, { kind: 'storageFull', httpStatus: null, retryAtMs: null }))
            .toBe(strings.freeSpace)
        expect(externalErrorMessage(strings, { code: 'corrupt', message: 'x', action: 'none', retryable: false }))
            .toBe(strings.corrupted)
    })

    it('names a repository this device has already connected', () => {
        expect(externalErrorMessage(strings, { kind: 'alreadyConnected' }))
            .toBe(strings.connectionAlreadyAdded)
    })

    it('reports unsupported operations without asking users to change the strategy', () => {
        const refused = { kind: 'unsupported', httpStatus: null, retryAtMs: null }
        expect(externalErrorMessage(strings, refused)).toBe(strings.unsupportedOperation)
    })

    it('shows the OAuth error code and provider description', () => {
        expect(externalErrorMessage(strings, {
            kind: 'reauthRequired', httpStatus: 400, retryAtMs: null,
            oauthError: 'invalid_grant', oauthErrorDescription: 'Bad Request: redirect_uri is invalid.',
        })).toBe([
            strings.reauthenticate,
            strings.oauthErrorCode.replace('{0}', 'invalid_grant'),
            strings.oauthErrorDescription.replace('{0}', 'Bad Request: redirect_uri is invalid.'),
        ].join('\n'))
        expect(externalErrorMessage(strings, {
            kind: 'reauthRequired', oauthError: '<script>alert(1)</script>',
        })).toBe([
            strings.reauthenticate,
            strings.oauthErrorCode.replace('{0}', '<script>alert(1)</script>'),
        ].join('\n'))
    })

    it('falls back for a local failure that carries no native kind', () => {
        expect(externalErrorMessage(strings, new Error('offline'))).toBe(strings.errorGeneric)
        expect(externalErrorMessage(strings, undefined)).toBe(strings.errorGeneric)
        expect(externalErrorMessage(strings, { kind: 'invented' })).toBe(strings.errorGeneric)
    })

    it('keeps both languages complete for every kind it maps', () => {
        const korean = externalStorageStrings('ko')
        const kinds = [
            'unauthorized', 'reauthRequired', 'notFound', 'preconditionFailed',
            'rateLimited', 'dailyQuotaExhausted', 'storageFull', 'fileTooLarge',
            'corrupt', 'unsupported', 'cancelled', 'transient', 'alreadyConnected',
            'folderNameConflict', 'folderCreateFailed', 'folderInaccessible',
            'folderNotRepository', 'folderUnsupportedLocation',
        ]
        for (const kind of kinds) {
            expect(externalErrorMessage(korean, { kind })).not.toBe(korean.errorGeneric)
            expect(externalErrorMessage(korean, { kind })).not.toBe(externalErrorMessage(strings, { kind }))
        }
    })

    it('separates folder-row failures from creation and connection failures', () => {
        for (const kind of ['folderInaccessible', 'folderNotRepository', 'folderUnsupportedLocation']) {
            expect(externalFolderErrorKind({ kind })).toBe(true)
        }
        for (const kind of ['folderNameConflict', 'notFound', undefined]) {
            expect(externalFolderErrorKind(kind === undefined ? undefined : { kind })).toBe(false)
        }
    })
})
