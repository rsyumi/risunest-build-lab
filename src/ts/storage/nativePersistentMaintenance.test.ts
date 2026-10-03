import { beforeEach, describe, expect, it, vi } from 'vitest'

const mocks = vi.hoisted(() => ({
    invoke: vi.fn(),
    isTauriMobile: true,
    relaunch: vi.fn(),
    confirmReplacement: vi.fn(),
    prepareReplacement: vi.fn(),
    acquirePause: vi.fn(),
    invalidatePlugins: vi.fn(),
    restartPlugins: vi.fn(),
    fencePlugins: vi.fn(),
    lifecycle: vi.fn(),
    continuation: vi.fn(),
}))

const bodyReceipt = {jobId:'bodies',kind:'snapshot-bodies',stagingId:'stage',activationRevision:'8',bindingAuthority:'0'}
const bodyStatus = {jobId:'bodies',kind:'snapshot-bodies',snapshotStagingId:'stage',activationRevision:8,activationAuthority:'0',state:'succeeded',phase:'complete',progress:{completedBytes:0,completedItems:0},snapshotBodies:{stageId:'stage',activatedRevision:8,bindingAuthority:'0',policy:'full',total:0,locallyPresent:0,remoteHeld:0,unavailable:0,allBodiesLocal:true,settled:true}}

vi.mock('@tauri-apps/api/core', () => ({ invoke: mocks.invoke }))
vi.mock('../desktopRelaunch', () => ({ relaunch: mocks.relaunch }))
vi.mock('./persistentDataRuntime.svelte',()=>({getPersistentDataRuntime:()=>({setActivatedLibraryRecoveryLifecycle:mocks.lifecycle,markCommittedWorkingSetRefreshRequired:vi.fn(),getStorageAuthorityEpoch:()=>4})}))
vi.mock('./sync/bindingRegistry',()=>({prepareBoundLibraryReplacement:mocks.prepareReplacement}))
vi.mock('./upstreamReplacement',()=>({acquireUpstreamImportPause:mocks.acquirePause,confirmUpstreamLibraryReplacement:mocks.confirmReplacement}))
vi.mock('../plugins/apiV3/v3.svelte',()=>({fencePluginExecutionForAuthorityReplacement:mocks.fencePlugins,invalidatePluginCachesAfterAuthorityReplacement:mocks.invalidatePlugins,restartPluginsAfterAuthorityReplacement:mocks.restartPlugins}))
vi.mock('./committedWorkingSetContinuation',()=>({registerCommittedWorkingSetContinuation:mocks.continuation}))
vi.mock('../platform', () => ({
    isTauriIOS: false,
    isTauriAndroid: false,
    get isTauriMobile() {
        return mocks.isTauriMobile
    },
}))

import {
    applyNativeDataHealthRepair,
    previewNativeDataHealthRepair,
    undoNativeDataHealthRepair,
    deleteNativePersistentSnapshot,
    executeNativePersistentAssetGc,
    getNativePersistentStorageStats,
    previewNativePersistentAssetGc,
    checkpointNativePersistentStore,
    createNativePersistentSnapshot,
    createPeriodicNativeSnapshotIfDue,
    listNativePersistentSnapshots,
    requestNativePersistentSnapshotRestore,
    completeNativeSnapshotRestoreBodies,
    restartNativeApp,
    restoreNativePersistentSnapshot,
    schedulePeriodicNativeSnapshot,
} from './nativePersistentMaintenance'

describe('native persistent maintenance', () => {
    it('sends the viewed diagnosis identity and expected revision for repair and undo', async () => {
        await previewNativeDataHealthRepair(['repair'], 123)
        await applyNativeDataHealthRepair(['repair'], true, 7, 123)
        await undoNativeDataHealthRepair('journal', 8)
        expect(mocks.invoke.mock.calls).toEqual([
            ['pds_data_health_repair_preview', { selection: ['repair'], expectedScannedAt: 123 }],
            ['pds_data_health_repair_apply', { selection: ['repair'], snapshot: true, expectedRevision: 7, expectedScannedAt: 123 }],
            ['pds_data_health_undo', { journalId: 'journal', expectedRevision: 8 }],
        ])
    })
    beforeEach(() => {
        mocks.invoke.mockReset().mockImplementation(async command => {
            if (command === 'native_snapshot_restore_bodies_start') return bodyReceipt
            if (command === 'native_file_job_status') return bodyStatus
        })
        mocks.isTauriMobile = true
        mocks.relaunch.mockReset()
        mocks.confirmReplacement.mockReset().mockResolvedValue(true)
        mocks.prepareReplacement.mockReset().mockResolvedValue({bound:false,state:{targetAuthority:'0'},fence:vi.fn(),assertAuthority:vi.fn(),resume:vi.fn()})
        mocks.acquirePause.mockReset().mockResolvedValue({token:{},fence:{revision:7,refreshCommittedWorkingSet:vi.fn()},complete:vi.fn(),finish:vi.fn(),abortUnchanged:vi.fn()})
        mocks.lifecycle.mockReset()
        mocks.continuation.mockReset()
        mocks.invalidatePlugins.mockReset()
        mocks.restartPlugins.mockReset()
        mocks.fencePlugins.mockReset()
        vi.useRealTimers()
        vi.unstubAllGlobals()
        delete (window as Window & {
            RisuLifecycleBridge?: unknown
        }).RisuLifecycleBridge
    })

    it('maps checkpoints to the native persistent store command', async () => {
        mocks.invoke.mockResolvedValue(undefined)

        await checkpointNativePersistentStore('truncate')

        expect(mocks.invoke).toHaveBeenCalledWith('pds_checkpoint', { mode: 'truncate' })
    })

    it('maps snapshot operations to native persistent store commands', async () => {
        const created = { id: 'ab18b8a5-f45c-46ba-bbf9-74b2cae87717', bytes: 1024, durationMs: 8 }
        const snapshots = [{ id: 'ab18b8a5-f45c-46ba-bbf9-74b2cae87717', bytes: 1024, modifiedAt: 123 }]
        mocks.invoke
            .mockResolvedValueOnce(created)
            .mockResolvedValueOnce(snapshots)
            .mockResolvedValueOnce({stagingId:'stage'})
            .mockResolvedValueOnce({revision:8})

        await expect(createNativePersistentSnapshot('periodic')).resolves.toEqual(created)
        await expect(listNativePersistentSnapshots()).resolves.toEqual(snapshots)
        await requestNativePersistentSnapshotRestore('ab18b8a5-f45c-46ba-bbf9-74b2cae87717')

        expect(mocks.invoke.mock.calls).toEqual([
            ['pds_snapshot_create', { reason: 'periodic' }],
            ['pds_snapshot_list'],
            ['pds_snapshot_restore_stage', { id: 'ab18b8a5-f45c-46ba-bbf9-74b2cae87717',requestId:expect.any(String) }],
            ['pds_snapshot_restore_activate',{stagingId:'stage',expectedRevision:7,bindingAuthority:'0'}],
            ['native_snapshot_restore_bodies_start',{stagingId:'stage',activationRevision:'8',bindingAuthority:'0'}],
            ['native_file_job_status',{jobId:'bodies'}],
            ['native_file_job_status',{jobId:'bodies'}],
        ])
    })

    it('maps storage maintenance commands through their typed invoke boundary', async () => {
        mocks.invoke.mockResolvedValue({})

        await getNativePersistentStorageStats()
        await deleteNativePersistentSnapshot('ab18b8a5-f45c-46ba-bbf9-74b2cae87717')
        await previewNativePersistentAssetGc()
        await executeNativePersistentAssetGc()

        expect(mocks.invoke.mock.calls).toEqual([
            ['pds_storage_stats'],
            ['pds_snapshot_delete', { id: 'ab18b8a5-f45c-46ba-bbf9-74b2cae87717' }],
            ['pds_asset_gc_preview'],
            ['pds_asset_gc_execute'],
        ])
    })


    it.each([
        { age: 24 * 60 * 60 * 1000 - 1, due: false },
        { age: 24 * 60 * 60 * 1000, due: true },
        { age: 24 * 60 * 60 * 1000 + 1, due: true },
    ])(
        'creates a periodic snapshot only at or beyond the 24-hour boundary',
        async ({ age, due }) => {
            const now = 2_000_000_000
            const created = { path: 'periodic.db', bytes: 2048, durationMs: 10 }
            mocks.invoke
                .mockResolvedValueOnce([
                    { path: 'previous.db', bytes: 1024, modifiedAt: now - age },
                ])
                .mockResolvedValueOnce(created)

            const result = await createPeriodicNativeSnapshotIfDue(now)

            expect(result).toEqual(due ? created : null)
            expect(mocks.invoke.mock.calls).toEqual(
                due
                    ? [['pds_snapshot_list'], ['pds_snapshot_create', { reason: 'periodic' }]]
                    : [['pds_snapshot_list']],
            )
        },
    )

    it('creates the first periodic snapshot when none exist', async () => {
        const created = { path: 'first.db', bytes: 512, durationMs: 4 }
        mocks.invoke.mockResolvedValueOnce([]).mockResolvedValueOnce(created)

        await expect(createPeriodicNativeSnapshotIfDue()).resolves.toEqual(created)
    })

    it('creates a periodic snapshot when the newest archive timestamp is in the future', async () => {
        const now = 2_000_000_000
        const created = { path: 'clock-recovered.db', bytes: 512, durationMs: 4 }
        mocks.invoke
            .mockResolvedValueOnce([{ path: 'future.db', bytes: 1, modifiedAt: now + 7 * 24 * 60 * 60 * 1000 }])
            .mockResolvedValueOnce(created)

        await expect(createPeriodicNativeSnapshotIfDue(now)).resolves.toEqual(created)
        expect(mocks.invoke).toHaveBeenLastCalledWith('pds_snapshot_create', {
            reason: 'periodic',
        })
    })

    it('registers one idle callback plus the hourly re-check and runs maintenance', async () => {
        let idleCallback: (() => void) | undefined
        const requestIdleCallback = vi.fn((callback: () => void) => {
            idleCallback = callback
            return 1
        })
        vi.stubGlobal('requestIdleCallback', requestIdleCallback)
        const setIntervalSpy = vi.fn()
        vi.stubGlobal('setInterval', setIntervalSpy)
        mocks.invoke.mockResolvedValueOnce([
            { path: 'recent.db', bytes: 1, modifiedAt: Date.now() },
        ])

        schedulePeriodicNativeSnapshot()

        expect(requestIdleCallback).toHaveBeenCalledTimes(1)
        expect(setIntervalSpy).toHaveBeenCalledWith(expect.any(Function), 60 * 60 * 1000)
        expect(mocks.invoke).not.toHaveBeenCalled()
        idleCallback?.()
        await vi.waitFor(() => expect(mocks.invoke.mock.calls).toEqual([
            ['pds_snapshot_list'],
            ['pds_message_object_sweep'],
        ]))
    })

    it('sweeps stored message objects even when the snapshot check fails', async () => {
        vi.useFakeTimers()
        vi.stubGlobal('requestIdleCallback', undefined)
        const consoleError = vi.spyOn(console, 'error').mockImplementation(() => undefined)
        const sweepError = new Error('sweep failed')
        mocks.invoke
            .mockRejectedValueOnce(new Error('snapshot list failed'))
            .mockRejectedValueOnce(sweepError)

        schedulePeriodicNativeSnapshot()
        await vi.advanceTimersByTimeAsync(0)

        expect(mocks.invoke.mock.calls).toEqual([['pds_snapshot_list'], ['pds_message_object_sweep']])
        expect(consoleError).toHaveBeenCalledWith('Periodic native object sweep failed', sweepError)
        consoleError.mockRestore()
    })

    it('falls back to a timer and logs maintenance failures', async () => {
        vi.useFakeTimers()
        vi.stubGlobal('requestIdleCallback', undefined)
        const error = new Error('snapshot list failed')
        const consoleError = vi.spyOn(console, 'error').mockImplementation(() => undefined)
        mocks.invoke.mockRejectedValueOnce(error)

        schedulePeriodicNativeSnapshot()
        await vi.advanceTimersByTimeAsync(0)

        expect(consoleError).toHaveBeenCalledWith('Periodic native snapshot failed', error)
        consoleError.mockRestore()
    })

    it('re-checks hourly so long sessions still create periodic snapshots', async () => {
        vi.useFakeTimers()
        vi.stubGlobal('requestIdleCallback', undefined)
        mocks.invoke.mockImplementation(async (command: string) =>
            command === 'pds_snapshot_list' ? [{ path: 'recent.db', bytes: 1, modifiedAt: Date.now() }] : undefined)
        const calls = (command: string) => mocks.invoke.mock.calls.filter(([name]) => name === command).length

        schedulePeriodicNativeSnapshot()
        await vi.advanceTimersByTimeAsync(0)
        expect([calls('pds_snapshot_list'), calls('pds_message_object_sweep')]).toEqual([1, 1])

        await vi.advanceTimersByTimeAsync(60 * 60 * 1000)
        expect([calls('pds_snapshot_list'), calls('pds_message_object_sweep')]).toEqual([2, 2])
    })

    it('reports an empty snapshot list without prompting', async () => {
        const actions = {
            choose: vi.fn(),
            onEmpty: vi.fn(),
        }
        mocks.invoke.mockResolvedValueOnce([])

        await expect(restoreNativePersistentSnapshot(actions)).resolves.toBe(false)

        expect(actions.onEmpty).toHaveBeenCalledOnce()
        expect(actions.choose).not.toHaveBeenCalled()
    })

    it('stops when snapshot choice or confirmation is cancelled', async () => {
        const snapshot = { id: 'snapshot.db', bytes: 1, modifiedAt: 2 }
        const baseActions = {
            onEmpty: vi.fn(),
        }
        mocks.invoke.mockResolvedValueOnce([snapshot])

        await expect(
            restoreNativePersistentSnapshot({
                ...baseActions,
                choose: vi.fn().mockResolvedValue(null),
            }),
        ).resolves.toBe(false)

        mocks.confirmReplacement.mockResolvedValueOnce(false)
        mocks.invoke.mockResolvedValueOnce([snapshot]).mockResolvedValueOnce({stagingId:'cancelled-stage'}).mockResolvedValueOnce(undefined)
        await expect(
            restoreNativePersistentSnapshot({
                ...baseActions,
                choose: vi.fn().mockResolvedValue(snapshot.id),
            }),
        ).resolves.toBe(false)

        expect(mocks.invoke.mock.calls).toEqual([['pds_snapshot_list'], ['pds_snapshot_list'],['pds_snapshot_restore_stage',{id:snapshot.id,requestId:expect.any(String)}],['pds_snapshot_restore_abort',{stagingId:'cancelled-stage'}]])
        expect(mocks.relaunch).not.toHaveBeenCalled()
    })

    it('rejects a path outside the returned snapshot list', async () => {
        mocks.invoke.mockResolvedValueOnce([{ path: 'allowed.db', bytes: 1, modifiedAt: 2 }])

        await expect(
            restoreNativePersistentSnapshot({
                choose: vi.fn().mockResolvedValue('other.db'),
                onEmpty: vi.fn(),
            }),
        ).rejects.toThrow('Selected native snapshot is not available')

        expect(mocks.invoke).toHaveBeenCalledTimes(1)
    })

    it('adopts the committed snapshot under the shared guard and reloads plugins', async () => {
        mocks.invoke.mockResolvedValueOnce([{id:'snapshot.db',bytes:1,modifiedAt:2}]).mockResolvedValueOnce({stagingId:'stage'}).mockResolvedValueOnce({revision:8})
        await expect(restoreNativePersistentSnapshot({choose:async()=>'snapshot.db',onEmpty:vi.fn()})).resolves.toBe(true)
        const pause=await mocks.acquirePause.mock.results[0].value
        const binding=await mocks.prepareReplacement.mock.results[0].value
        expect(pause.fence.refreshCommittedWorkingSet).toHaveBeenCalledWith(8)
        expect(mocks.invalidatePlugins).toHaveBeenCalledOnce()
        expect(mocks.restartPlugins).toHaveBeenCalledOnce()
        expect(pause.complete).toHaveBeenCalledOnce()
        expect(binding.resume).toHaveBeenCalledOnce()
        expect(mocks.relaunch).not.toHaveBeenCalled()
        expect(mocks.lifecycle).toHaveBeenCalledOnce()
    })
    it('retains exact activation recovery after a lost receipt', async () => {
        const lost=new Error('receipt lost')
        mocks.invoke.mockResolvedValueOnce({stagingId:'stage'}).mockRejectedValueOnce(lost).mockResolvedValueOnce({revision:8})
        await expect(requestNativePersistentSnapshotRestore('snapshot')).rejects.toBe(lost)
        expect(mocks.continuation).toHaveBeenCalledOnce()
        const lifecycle=mocks.lifecycle.mock.calls[0][1]
        await expect(lifecycle.beforeRefresh()).resolves.toBe(8)
        expect(mocks.invoke.mock.calls[1]).toEqual(mocks.invoke.mock.calls[2])
        expect(mocks.invoke.mock.calls.some(([command])=>command==='pds_snapshot_restore_abort')).toBe(false)
        await lifecycle.afterRefresh()
        expect(mocks.restartPlugins).toHaveBeenCalledOnce()
    })
    it('reports post-adoption body failure without activating again or requiring working-set recovery', async () => {
        const failed={...bodyStatus,state:'failed',error:{code:'snapshot-bodies-incomplete',message:'Snapshot library is restored, but some bodies remain unavailable'},snapshotBodies:{...bodyStatus.snapshotBodies,allBodiesLocal:false,settled:false,total:1,locallyPresent:0,unavailable:1}}
        mocks.invoke.mockImplementation(async command=>command==='native_snapshot_restore_bodies_start'?bodyReceipt:command==='native_file_job_status'?failed:undefined)
            .mockResolvedValueOnce({stagingId:'stage'}).mockResolvedValueOnce({revision:8})
        await expect(requestNativePersistentSnapshotRestore('snapshot')).rejects.toMatchObject({name:'NativeSnapshotBodiesCommittedError',bodies:{unavailable:1}})
        expect(mocks.continuation).not.toHaveBeenCalled()
        expect(mocks.invoke.mock.calls.filter(([command])=>command==='pds_snapshot_restore_activate')).toHaveLength(1)
        expect(mocks.invoke.mock.calls.some(([command])=>command==='pds_snapshot_restore_abort')).toBe(false)
        const binding=await mocks.prepareReplacement.mock.results[0].value
        expect(binding.resume).toHaveBeenCalledOnce()
        mocks.invoke.mockImplementation(async command=>command==='native_snapshot_restore_bodies_start'?bodyReceipt:bodyStatus)
        await expect(completeNativeSnapshotRestoreBodies('stage',8,'0')).resolves.toMatchObject({allBodiesLocal:true})
        expect(mocks.invoke.mock.calls.filter(([command])=>command==='pds_snapshot_restore_activate')).toHaveLength(1)
    })
    it('keeps the committed snapshot identity after a lost body-start response', async () => {
        mocks.invoke.mockResolvedValueOnce({stagingId:'stage'}).mockResolvedValueOnce({revision:8}).mockRejectedValueOnce(new Error('start response lost'))
        await expect(requestNativePersistentSnapshotRestore('snapshot')).rejects.toMatchObject({
            name:'NativeSnapshotBodiesCommittedError',code:'snapshot-body-start-unknown',
            receipt:{stagingId:'stage',activationRevision:'8',bindingAuthority:'0'},
        })
        expect(mocks.continuation).not.toHaveBeenCalled()
        expect(mocks.invoke.mock.calls.some(([command])=>command==='pds_snapshot_restore_abort')).toBe(false)
        mocks.invoke.mockImplementation(async command=>command==='native_snapshot_restore_bodies_start'?bodyReceipt:bodyStatus)
        await expect(completeNativeSnapshotRestoreBodies('stage',8,'0')).resolves.toMatchObject({allBodiesLocal:true})
        expect(mocks.invoke.mock.calls.filter(([command])=>command==='pds_snapshot_restore_activate')).toHaveLength(1)
    })
    it('does not restart when snapshot staging fails', async () => {
        const requestError = new Error('stage failed')
        const restart = vi.fn()
        mocks.invoke
            .mockResolvedValueOnce([{ id: 'snapshot.db', bytes: 1, modifiedAt: 2 }])
            .mockRejectedValueOnce(requestError)

        await expect(
            restoreNativePersistentSnapshot({
                choose: vi.fn().mockResolvedValue('snapshot.db'),
                onEmpty: vi.fn(),
            }),
        ).rejects.toBe(requestError)

        expect(restart).not.toHaveBeenCalled()
    })

    it('requests an Android cold restart through the lifecycle bridge', async () => {
        const requestRestart = vi.fn()
        ;(window as Window & {
            RisuLifecycleBridge?: { requestRestart(): void }
        }).RisuLifecycleBridge = { requestRestart }

        await restartNativeApp()

        expect(requestRestart).toHaveBeenCalledOnce()
        expect(mocks.relaunch).not.toHaveBeenCalled()
    })

    it('does not fall back to the unsupported process relauncher on Android', async () => {
        await expect(restartNativeApp()).rejects.toThrow(
            'Android restart bridge is unavailable',
        )

        expect(mocks.relaunch).not.toHaveBeenCalled()
    })

    it('uses the process relauncher outside Tauri mobile', async () => {
        mocks.isTauriMobile = false
        mocks.relaunch.mockResolvedValue(undefined)

        await restartNativeApp()

        expect(mocks.relaunch).toHaveBeenCalledOnce()
    })
})
