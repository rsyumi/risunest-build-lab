import { describe, expect, it, vi } from 'vitest'
import { appLock } from './appLock.svelte'
import { createPinRecord, defaultAppLockRecord } from './record'
import type { AppLockLifecycle, AppLockNativePolicy } from './native'

const boundary = vi.hoisted(() => ({
    snapshot: { active: true, epoch: 0, atMs: 0, departedAtMs: null } as AppLockLifecycle,
    policy: null as AppLockNativePolicy | null,
    visible: false,
    rejectedFrames: 1,
    listener: null as ((event: { payload: AppLockLifecycle }) => void) | null,
}))
vi.mock('@tauri-apps/api/core', async (original) => ({
    ...await original<typeof import('@tauri-apps/api/core')>(),
    invoke: async (command: string, args?: Record<string, unknown>) => {
        if (command === 'app_lock_check') return { auth: { status: 'unsupported' }, previewSupported: false }
        if (command === 'app_lock_set_policy') {
            boundary.policy = args!.policy as AppLockNativePolicy
            return boundary.snapshot
        }
        if (command === 'app_lock_ready') {
            if (args!.epoch !== boundary.snapshot.epoch) return false
            if (boundary.rejectedFrames-- > 0) return false
            boundary.visible = true
            return true
        }
        throw new Error(`Unexpected native command: ${command}`)
    },
}))
vi.mock('@tauri-apps/api/event', async (original) => ({
    ...await original<typeof import('@tauri-apps/api/event')>(),
    listen: async (_event: string, listener: (event: { payload: AppLockLifecycle }) => void) => {
        boundary.listener = listener
        return () => { if (boundary.listener === listener) boundary.listener = null }
    },
}))

describe('app lock startup facade', () => {
    it('releases a boot failure before marker initialization and restores lock policy before normal content', async () => {
        appLock.setBackgroundColor('rgb(255 255 255)')
        await appLock.renderBootFailure()
        expect(appLock.state.initialized).toBe(false)
        expect(boundary.visible).toBe(true)
        expect(boundary.policy).toMatchObject({ gateRequired: false, enabled: false, backgroundColor: '#ffffff' })
        boundary.visible = false
        boundary.snapshot = { active: true, epoch: 2, atMs: 100, departedAtMs: 50 }
        boundary.listener!({ payload: boundary.snapshot })
        await vi.waitFor(() => expect(boundary.visible).toBe(true))
        const pin = await createPinRecord('123456')
        const record = JSON.stringify({ ...defaultAppLockRecord(), enabled: true, pin })
        await appLock.initialize({ read: () => record, commit: async () => {} })
        expect(appLock.state.locked).toBe(true)
        expect(boundary.policy).toMatchObject({ gateRequired: true, enabled: true })
    })
})
