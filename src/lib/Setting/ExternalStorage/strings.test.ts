import { describe, expect, it } from 'vitest'
import { externalErrorMessage, externalFolderErrorKind, externalStorageStrings } from './strings'

const strings = externalStorageStrings('en')

describe('the storage usage copy', () => {
    it('names the repository data this device knows of without calling it a lower bound', () => {
        expect(externalStorageStrings('ko')).toMatchObject({ uploadedLowerBound: '이 기기에서 확인한 저장소 데이터', files: '파일 {0}개' })
        expect(strings).toMatchObject({ uploadedLowerBound: 'Repository data known to this device', files: '{0} files' })
    })

    it('says only what cleanup removes under the retention settings', () => {
        expect(externalStorageStrings('ko').retentionHelp).toBe('정리할 때 이 기기에서 만든 자동 백업 중 보관 개수와 보관 기간을 모두 넘긴 백업을 지웁니다.')
        expect(strings.retentionHelp).toBe('Cleanup removes automatic backups made on this device once they are past both the number of backups to keep and the days to keep them.')
    })
})

describe('externalErrorMessage', () => {
    it('reads the kind of a rejected command and the code of a job error', () => {
        expect(externalErrorMessage(strings, { kind: 'storageFull', httpStatus: null, retryAtMs: null }))
            .toBe(strings.freeSpace)
        expect(externalErrorMessage(strings, { code: 'corrupt', message: 'x', action: 'none', retryable: false }))
            .toBe(strings.corrupted)
    })

    it('tells a wrong recovery key apart from another repository at the location', () => {
        expect(externalErrorMessage(strings, { kind: 'recoveryKeyMismatch' })).toBe(strings.recoveryKeyMismatch)
        expect(externalErrorMessage(strings, { code: 'repositoryMismatch', message: 'x', action: 'none', retryable: false }))
            .toBe(strings.repositoryMismatch)
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
            'endpointRejected', 'deviceVaultUnavailable', 'clockSkew', 'folderTooLarge',
            'repositoryBusy', 'locationOccupied', 'authorizationTimedOut', 'localStorageFull',
            'localPermissionDenied', 'localFailure', 'repositoryKeyUnavailable', 'authorizationUnavailable',
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
