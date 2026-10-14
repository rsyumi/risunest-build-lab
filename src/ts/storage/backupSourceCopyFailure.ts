// Android and iOS report the same codes when a backup source copy fails. Codes without an import
// dialog reason are mapped to the reason that tells the user what to do next.
const DIALOG_CODES: Readonly<Record<string, string>> = {
    // The selection ended or the file changed during the copy, so only selecting it again can help.
    'source-changed': 'source-reselect-required',
    cancelled: 'source-reselect-required',
    'spool-create-failed': 'spool-write-failed',
}

export class BackupSourceCopyError extends Error {
    readonly code: string

    constructor(readonly nativeCode: string) {
        super(`Backup source copy failed: ${nativeCode}`)
        this.name = 'BackupSourceCopyError'
        this.code = Object.hasOwn(DIALOG_CODES, nativeCode) ? DIALOG_CODES[nativeCode] : nativeCode
    }
}

/** Gives a native rejection that carries a copy failure code its dialog reason. */
export function backupSourceCopyFailure(error: unknown): unknown {
    if (!error || typeof error !== 'object' || !('code' in error) || typeof error.code !== 'string') return error
    return new BackupSourceCopyError(error.code)
}
