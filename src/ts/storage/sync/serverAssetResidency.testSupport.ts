import type { AssetResidencyStatus } from './serverAssetResidency'

/** A native asset residency status that the status reader accepts, including target-scoped reads. */
export function assetResidencyStatus(overrides: Partial<AssetResidencyStatus> = {}): AssetResidencyStatus {
    return {
        policy: 'full',
        localBytes: 0,
        remoteBytes: 0,
        remoteObjects: 0,
        serverBytes: 0,
        serverObjects: 0,
        externalObjects: [],
        unavailableObjects: 0,
        evictedBytes: 0,
        previousStorageObjects: 0,
        ...overrides,
    }
}
