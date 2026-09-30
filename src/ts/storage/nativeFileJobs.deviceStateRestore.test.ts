import { afterEach, expect, it, vi } from 'vitest'

const effects = vi.hoisted(() => ({ hub: vi.fn(), log: vi.fn(async () => {}) }))
vi.mock('../platform', () => ({ isTauri: true, isTauriIOS: false }))
vi.mock('../characterCards', () => ({ applyHubSelection: effects.hub }))
vi.mock('../nativeLog', () => ({ setNativeLogFileEnabled: effects.log }))

import { runNativeArchiveRestore, type NativeFileJobStatus } from './nativeFileJobs'
import { initializeDeviceMarkers, installDeviceMarkers, getDeviceMarkers } from './deviceMarkers'
import { reloadDeviceSettings, getDeviceSettings, updateDeviceSettings } from './deviceSettings'
import { getAppUpdateSettings, reloadAppUpdateSettings, updateAppUpdateSettings,
    subscribeAppUpdateSettings } from '../update/settings'

afterEach(() => { installDeviceMarkers(null); vi.clearAllMocks() })

it('clears cached update and device preferences through an empty native settings restore before releasing the fence', async () => {
    const durable = new Map<string, unknown>()
    const markers = await initializeDeviceMarkers({
        get: async key => durable.get(key) ?? null,
        readMany: async keys => keys.map(key => durable.get(key) ?? null),
        set: async (key, value) => { if (value === null) durable.delete(key); else durable.set(key, value) },
        patch: async () => { throw new Error('Unexpected settings patch') },
    })
    reloadAppUpdateSettings()
    reloadDeviceSettings()
    updateAppUpdateSettings({ autoUpdateCheck: false, skippedVersion: '2.3.4' })
    updateDeviceSettings({ nativeFileLogEnabled: false, performanceProfile: 'low-spec' })
    expect(getAppUpdateSettings().autoUpdateCheck).toBe(false)
    const updates = vi.fn()
    const unsubscribe = subscribeAppUpdateSettings(updates)
    const events: string[] = []
    let fenced = false
    const assertRestored = () => {
        expect(getDeviceMarkers()).toBe(markers)
        expect(markers.getItem('risuNestUpdateSettings')).toBeNull()
        expect(markers.getItem('risuNestDeviceSettings')).toBeNull()
        expect(getAppUpdateSettings()).toMatchObject({ autoUpdateCheck: true, skippedVersion: '' })
        expect(getDeviceSettings()).toMatchObject({ nativeFileLogEnabled: true, performanceProfile: 'normal' })
    }
    const result = { revision: 4, sourceBytes: 128, sourceSha256: 'a'.repeat(64),
        characterCount: 0, presetCount: 0, warningCodes: [] }
    const base = { jobId: 'settings-restore', kind: 'restore-portable-backup' as const,
        progress: { completedBytes: 128, totalBytes: 128, completedItems: 0 } }
    const statuses: NativeFileJobStatus[] = [
        { ...base, state: 'waitingForInput', phase: 'awaiting-backup-selection',
            restorePreview: { libraryIncluded: false, repairRequired: false, deviceSections: ['local-settings'] } },
        { ...base, state: 'waitingForInput', phase: 'awaiting-activation' },
        { ...base, state: 'succeeded', phase: 'complete', result, deviceSessionId: 'empty-settings' },
    ]
    const runtime: Parameters<typeof runNativeArchiveRestore>[0] = {
        getStorageAuthorityEpoch: () => 1,
        capturePersistentMutationToken: async () => ({ revision: 3, mutationGeneration: 1 }),
        acquireDestructiveReplacementFence: async () => {
            fenced = true
            return { revision: 3,
                refreshCommittedWorkingSet: async revision => {
                    expect(fenced).toBe(true)
                    assertRestored()
                    events.push('projection')
                    return { kind: 'committed', revision, projection: 'applied' }
                },
                release: () => { assertRestored(); fenced = false; events.push('release') },
            }
        },
        markCommittedWorkingSetRefreshRequired: () => { throw new Error('Unexpected committed refresh failure') },
    }
    try {
        await expect(runNativeArchiveRestore(runtime, { type: 'desktopPath', path: '/synthetic/empty-settings.risunest' }, {
            choosePortableSections: async () => ({ library: false, deviceSections: ['local-settings'] }),
        }, {
            isTauri: () => true,
            invoke: async command => {
                if (command === 'native_file_job_start') return { jobId: base.jobId }
                if (command === 'native_file_job_status') return statuses.shift()
                if (command === 'native_portable_select_sections') return undefined
                if (command === 'native_file_job_finalize') {
                    expect(fenced).toBe(true)
                    expect(durable.has('risuNestUpdateSettings')).toBe(true)
                    expect(durable.has('risuNestDeviceSettings')).toBe(true)
                    durable.clear()
                    // The native section is empty, while both renderer caches still hold prior values.
                    expect(markers.getItem('risuNestUpdateSettings')).not.toBeNull()
                    expect(getAppUpdateSettings().skippedVersion).toBe('2.3.4')
                    events.push('activate')
                    return undefined
                }
                if (command === 'native_device_backup_recovery_complete') { events.push('ack'); return undefined }
                if (command === 'pds_open') return { revision: 4 }
                if (command === 'native_file_job_forget') { events.push('forget'); return true }
                throw new Error(`Unexpected native command: ${command}`)
            },
            wait: async () => {},
        })).resolves.toEqual(result)
        expect(events).toEqual(['activate', 'ack', 'projection', 'release', 'forget'])
        expect(updates).toHaveBeenLastCalledWith(expect.objectContaining({ autoUpdateCheck: true, skippedVersion: '' }))
        expect(effects.log).toHaveBeenCalledExactlyOnceWith(true)
        expect(effects.hub).toHaveBeenCalledExactlyOnceWith(markers)
    } finally { unsubscribe() }
})
