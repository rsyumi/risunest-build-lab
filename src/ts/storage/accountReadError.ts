export class AccountReadError extends Error {
    constructor(
        readonly kind: 'http' | 'transport' | 'timeout' | 'malformed' | 'account-changed' | 'local',
        readonly status?: number,
        readonly retryAfter?: string,
    ) {
        super(kind === 'account-changed' ? 'Official account session changed during read'
            : status === undefined ? `Official account read failed: ${kind}` : `Official account read failed: HTTP ${status}`)
        this.name = 'AccountReadError'
    }
}

export class OfficialAssetOwnershipError extends Error {
    constructor(readonly reason: 'account-changed' | 'library-changed' | 'receipt') { super(`Official account continuation is unavailable: ${reason}`) }
}
