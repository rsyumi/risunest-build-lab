import { vi } from 'vitest'
import type { AssetAliasCleanupTarget } from './deletedOwnerAssetCleanup'

/** A cleanup target whose save queue runs each storage-only mutation at once and keeps its revision. */
export function createAssetAliasCleanupTarget(flushPendingData = vi.fn(async (_reason: string) => {})) {
    let revision = 1
    return {
        runtime: {
            getStorageAuthorityEpoch: () => 0,
            flushPendingData,
            runStorageOnlyMutation: async (operation: (expectedRevision: number) => Promise<number>) => {
                revision = await operation(revision)
            },
        },
        gate: { runWrite: <T>(operation: () => Promise<T>) => operation() },
    } satisfies AssetAliasCleanupTarget
}
