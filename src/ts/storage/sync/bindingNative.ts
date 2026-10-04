import { invoke } from '@tauri-apps/api/core'
import type { SyncBindingNative, SyncBindingState, StagedSyncTarget, BindingContext, NewDeviceBindingPreparation, NewDeviceBindingResult } from './bindingFlow'

export function createNativeSyncBindingBridge(): SyncBindingNative {
    const state = () => invoke<SyncBindingState>('pds_lww_binding_state')
    return {
        state,
        async assertAuthority(expected) {
            const current = await state()
            if (current.targetAuthority !== expected.targetAuthority || current.selectionEpoch !== expected.selectionEpoch) {
                throw new Error('Sync binding changed')
            }
        },
        switchTarget: (expected, target, inspection, requestId, initialPublication) => invoke<SyncBindingState>('pds_lww_switch_target', {
            request: { bindingAuthority: expected.targetAuthority, requestId, expectedSelectionEpoch: expected.selectionEpoch, target, inspectionId: inspection?.inspectionId ?? null, initialPublication },
        }),
    }
}

export async function replaceNativeSyncBinding(staged: StagedSyncTarget, context: BindingContext): Promise<void> {
    context.signal.throwIfAborted()
    await invoke('pds_lww_replace_from_target', { request: {
        bindingAuthority: context.state.targetAuthority,
        requestId: staged.receiveId,
        expectedSelectionEpoch: context.state.selectionEpoch,
        stagingId: staged.stagingId,
        receiveId: staged.receiveId,
        targetId: staged.targetId,
        libraryId: staged.libraryId,
    } })
    context.signal.throwIfAborted()
}

export async function replaceNativeSyncBindingAsNewDevice(staged: StagedSyncTarget, preparation: NewDeviceBindingPreparation, context: BindingContext): Promise<NewDeviceBindingResult> {
    context.signal.throwIfAborted()
    const result = await invoke<NewDeviceBindingResult>('pds_lww_replace_target_as_new_device', { request: {
        bindingAuthority: context.state.targetAuthority,
        requestId: staged.receiveId,
        stagingId: staged.stagingId,
        authorizationId: preparation.authorizationId,
    } })
    context.signal.throwIfAborted()
    return result
}
