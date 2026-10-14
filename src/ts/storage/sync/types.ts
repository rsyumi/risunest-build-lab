export const officialAccountSnapshotCapability = {
    kind: 'official-account-snapshot',
    database: 'full-risu-save',
    assets: 'read-write-replacement-key',
    coldStorage: 'read-only-keys',
    remoteRevision: 'opaque-session-cache',
} as const

export type SyncCapability = typeof officialAccountSnapshotCapability
