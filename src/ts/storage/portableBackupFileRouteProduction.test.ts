import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { clearMocks, mockIPC } from '@tauri-apps/api/mocks'
const m = vi.hoisted(() => ({
    invoke: vi.fn(),
    open: vi.fn(),
    save: vi.fn(),
    chooseExport: vi.fn(),
    chooseRestore: vi.fn(),
    confirm: vi.fn(),
    portable: vi.fn(),
    block: vi.fn(),
    legacy: vi.fn(),
    fallback: vi.fn(),
    status: vi.fn(),
    export: vi.fn(),
    discard: vi.fn(),
    discardIOS: vi.fn(),
    materializeAndroid: vi.fn(),
    materializeIOS: vi.fn(),
    beginReplacement: vi.fn(),
    releaseReplacement: vi.fn(),
    confirmReplacement: vi.fn(),
    after: vi.fn(),
    resume: vi.fn(),
    hold: vi.fn(),
    manager: vi.fn(),
    controller: new AbortController(),
    runtime: { revision: 4 },
}))
vi.mock('../drive/backuplocal', () => ({ importLegacyBackupWithWebView: m.fallback }))
vi.mock('@tauri-apps/api/core', async original => ({ ...(await original<typeof import('@tauri-apps/api/core')>()), invoke: m.invoke } satisfies typeof import('@tauri-apps/api/core')))
vi.mock('@tauri-apps/plugin-dialog', () => ({ open: m.open, save: m.save }))
vi.mock('../platform', async () => (await import('../platform.testSupport')).platformModule({ runtime: 'tauri', os: 'windows' }))
vi.mock('../alert', () => ({ alertConfirm: m.confirm, alertNormal: vi.fn() }))
vi.mock('../plugins/plugins.svelte', () => ({
    loadPluginsAfterAuthoritativeRestore: m.after,
}))
vi.mock('./androidSafBridge', () => ({
    discardAndroidSafSource: m.discard,
    pickAndroidPortableBackupSource: vi.fn(),
    materializeAndroidBackupSource: m.materializeAndroid,
}))
vi.mock('./iosFiles', async (original) => ({...(await original<object>()),pickIOSBackupSource:vi.fn(),materializeIOSBackupSource:m.materializeIOS,discardIOSFile:m.discardIOS}))
vi.mock('./deviceBackup/selectionDialog', () => ({
    selectPortableBackupExport: m.chooseExport,
    selectPortableBackupRestore: m.chooseRestore,
}))
vi.mock('./nativeFileSourceInfo', () => ({
    describeDesktopSource: async () => ({ name: 'misleading.bin', bytes: 30 }),
}))
vi.mock('./persistentDataRuntime.svelte', () => ({
    getPersistentDataRuntime: () => m.runtime,
}))
vi.mock('./nativeFileJobManager', () => ({
    runSharedNativeFileOperation: m.manager,
}))
vi.mock('./sync/serverSyncProduction', () => ({
    getServerSyncController: () => ({
        assertFileOperationAvailable() {},
        beginReplacement: m.beginReplacement,
        confirmReplacement: m.confirmReplacement,
    }),
    resumeServerSyncAfterBackup: m.resume,
    holdServerSyncAfterRestore: m.hold,
}))
vi.mock('./nativeFileJobs', async (original) => ({
    ...(await original<object>()),
    runNativeArchiveExport: m.export,
    runNativeArchiveRestore: m.portable,
    runNativeBlockRisuSaveRestore: m.block,
    runNativeLegacyLocalBackupRestore: m.legacy,
}))
import {
    exportPortableBackupFromSystemPicker,
    restoreBackupFromNativeSource,
    restoreBackupFromSystemPicker,
} from './portableBackupFileRouteProduction.svelte'
const result = {
    revision: 5,
    sourceBytes: 30,
    sourceSha256: 'a'.repeat(64),
    characterCount: 1,
    presetCount: 1,
    warningCodes: [],
    sourceFingerprintKind: 'portable-catalog-sha256' as const,
}
describe('common backup file production route', () => {
    beforeEach(() => {
        vi.resetAllMocks()
        vi.stubGlobal('window', Object.assign(new EventTarget(), { crypto: globalThis.crypto }))
        mockIPC(() => undefined)
        m.controller = new AbortController()
        m.manager.mockImplementation(async (_kind, _label, run) =>
            run({
                signal: m.controller.signal,
                onStatus: m.status,
                setBlocking: vi.fn(),
                setSource: vi.fn(),
                setPartialWritesPossible: vi.fn(),
            }),
        )
        m.invoke.mockResolvedValue('portable')
        m.confirm.mockResolvedValue(true)
        m.beginReplacement.mockResolvedValue(m.releaseReplacement)
        for (const operation of [m.portable, m.block, m.legacy, m.export])
            operation.mockResolvedValue(result)
        m.chooseExport.mockResolvedValue({
            library: true,
            deviceSections: ['local-storage', 'localforage'],
        })
        m.save.mockResolvedValue('C:\\synthetic\\backup.risunest')
    })
    afterEach(() => { clearMocks(); vi.unstubAllGlobals() })
    it.each(['androidSeekable','iosScoped'] as const)('keeps fresh %s source seekable without materialization', async (type) => {
        const source={type,token:'synthetic-custody'}
        await expect(restoreBackupFromNativeSource(source)).resolves.toEqual(result)
        expect(m.portable.mock.calls[0][1]).toEqual(source)
        expect(m.materializeAndroid).not.toHaveBeenCalled()
        expect(m.materializeIOS).not.toHaveBeenCalled()
        expect(m.invoke).toHaveBeenLastCalledWith('native_portable_source_discard',{source})
    })
    it('materializes confirmed upstream Android source after format probe',async()=>{
        m.invoke.mockResolvedValue('local-backup')
        const bridge = {
            copyExport() {},
            materializeBackupSource(_token: string, report?: (progress: { copiedBytes: number; totalBytes: number | null }) => void) {
                report?.({ copiedBytes: 4 * 1024 ** 3, totalBytes: 8 * 1024 ** 3 })
                return JSON.stringify({ ready: [{ token: 'synthetic-materialized' }], failures: [] })
            },
        }
        const { materializeAndroidBackupSource } = await vi.importActual<typeof import('./androidSafBridge')>('./androidSafBridge')
        m.materializeAndroid.mockImplementation(async(token, options)=>{
            expect(m.invoke).toHaveBeenCalledWith('native_backup_source_format',{source:{type:'androidSeekable',token:'synthetic-custody'}})
            return materializeAndroidBackupSource(token, options, bridge)
        })
        await restoreBackupFromNativeSource({type:'androidSeekable',token:'synthetic-custody'})
        expect(m.status).toHaveBeenCalledWith(expect.objectContaining({
            detail: expect.objectContaining({
                stage: 'copying-source', stageUnit: 'bytes', stageCompleted: 4 * 1024 ** 3, stageTotal: 8 * 1024 ** 3,
            }),
        }))
        expect(m.legacy.mock.calls[0][1]).toEqual({type:'androidSpool',token:'synthetic-materialized'})
        expect(m.discard).toHaveBeenCalledWith('synthetic-materialized')
    })
    it('propagates cancellation into the Android copy and removes its late result before restore', async () => {
        m.invoke.mockResolvedValue('local-backup')
        const released: string[] = []
        const removed: string[] = []
        const bridge = {
            copyExport() {},
            async materializeBackupSource() {
                m.controller.abort()
                await Promise.resolve()
                return JSON.stringify({ ready: [{ token: 'late-copy' }], failures: [] })
            },
            discardPortableSource(token: string) { released.push(token); return true },
            discardSource(token: string) { removed.push(token); return true },
        }
        const { materializeAndroidBackupSource } = await vi.importActual<typeof import('./androidSafBridge')>('./androidSafBridge')
        m.materializeAndroid.mockImplementation((token, options) => materializeAndroidBackupSource(token, options, bridge))
        await expect(restoreBackupFromNativeSource({ type: 'androidSeekable', token: 'synthetic-custody' }))
            .rejects.toMatchObject({ name: 'AbortError' })
        expect(released).toEqual(['synthetic-custody'])
        expect(removed).toEqual(['late-copy'])
        expect(m.legacy).not.toHaveBeenCalled()
        expect(m.invoke).toHaveBeenLastCalledWith('native_portable_source_discard', { source: { type: 'androidSeekable', token: 'synthetic-custody' } })
    })
    it.each(['androidSeekable', 'iosScoped'] as const)('settles a %s restore cancelled during its copy without waiting for the copy reply', async type => {
        const source = { type, token: 'synthetic-custody' }
        let reply!: (value: unknown) => void
        const copyReply = new Promise(resolve => { reply = resolve })
        let copying = false
        const released: string[] = []
        const removed: string[] = []
        const { materializeAndroidBackupSource } = await vi.importActual<typeof import('./androidSafBridge')>('./androidSafBridge')
        const { materializeIOSBackupSource } = await vi.importActual<typeof import('./iosFiles')>('./iosFiles')
        m.materializeAndroid.mockImplementation((token, options) => materializeAndroidBackupSource(token, options, {
            copyExport() {},
            materializeBackupSource() { copying = true; return copyReply as Promise<string> },
            discardPortableSource(token) { released.push(token); return true },
            discardSource(token) { removed.push(token); return true },
        }))
        m.materializeIOS.mockImplementation(materializeIOSBackupSource)
        m.invoke.mockImplementation(async (command, args) => {
            if (command === 'native_backup_source_format') return 'local-backup'
            if (command === 'plugin:ios-native|materialize_backup_source') { copying = true; return copyReply }
            if (command === 'native_portable_source_discard' && type === 'iosScoped') released.push(args.source.token)
            if (command === 'plugin:ios-native|discard_file') removed.push(args.path)
            return true
        })
        const restoring = restoreBackupFromNativeSource(source)
        await vi.waitFor(() => expect(copying).toBe(true))
        m.controller.abort()
        await expect(restoring).rejects.toMatchObject({ name: 'AbortError' })
        // Android releases through its control bridge, iOS through the same command the route uses.
        expect(released).toEqual(type === 'iosScoped' ? ['synthetic-custody', 'synthetic-custody'] : ['synthetic-custody'])
        expect(m.legacy).not.toHaveBeenCalled()
        expect(m.invoke).toHaveBeenLastCalledWith('native_portable_source_discard', { source })
        reply(type === 'iosScoped'
            ? { path: '/owned/late.risudat', name: 'late.risudat', bytes: 42 }
            : JSON.stringify({ ready: [{ token: 'late-copy' }], failures: [] }))
        await vi.waitFor(() => expect(removed).toEqual([type === 'iosScoped' ? '/owned/late.risudat' : 'late-copy']))
    })
    it.each([false, true])('reports iOS copy progress and propagates cancellation before restore (cancel %s)', async cancel => {
        const { materializeIOSBackupSource, discardIOSFile } = await vi.importActual<typeof import('./iosFiles')>('./iosFiles')
        m.materializeIOS.mockImplementation(materializeIOSBackupSource)
        const iosModule = await import('./iosFiles')
        vi.mocked(iosModule.discardIOSFile).mockImplementation(discardIOSFile)
        let finishCopy: (() => void) | undefined
        m.invoke.mockImplementation(async (command, args) => {
            if (command === 'native_backup_source_format') return 'local-backup'
            if (command === 'plugin:ios-native|materialize_backup_source') {
                args.onProgress.onmessage({ copiedBytes: 4 * 1024 ** 3, totalBytes: 8 * 1024 ** 3 })
                const copied = { path: '/owned/copied.risudat', name: 'copied.risudat', bytes: 8 * 1024 ** 3 }
                if (cancel) return new Promise(resolve => {
                    finishCopy = () => resolve(copied)
                    m.controller.abort()
                })
                return copied
            }
            if (command === 'native_portable_source_discard') finishCopy?.()
            return true
        })
        const restoring = restoreBackupFromNativeSource({ type: 'iosScoped', token: 'synthetic-custody' })
        if (cancel) await expect(restoring).rejects.toMatchObject({ name: 'AbortError' })
        else await expect(restoring).resolves.toEqual(result)
        expect(m.status).toHaveBeenCalledWith(expect.objectContaining({ detail: expect.objectContaining({
            stage: 'copying-source', stageUnit: 'bytes', stageCompleted: 4 * 1024 ** 3, stageTotal: 8 * 1024 ** 3,
        }) }))
        if (cancel) {
            expect(m.legacy).not.toHaveBeenCalled()
        } else {
            expect(m.legacy.mock.calls[0][1]).toEqual({ type: 'desktopPath', path: '/owned/copied.risudat' })
        }
        expect(m.invoke).toHaveBeenCalledWith('native_portable_source_discard', { source: { type: 'iosScoped', token: 'synthetic-custody' } })
        if (cancel) expect(m.invoke).toHaveBeenCalledWith('plugin:ios-native|discard_file', { path: '/owned/copied.risudat' })
    })
    it.each(['androidSeekable', 'iosScoped'] as const)('removes a delivered %s copy on page reload while a choice is pending', async type => {
        const source = { type, token: 'synthetic-custody' }
        m.invoke.mockImplementation(async command => command === 'native_backup_source_format' ? 'local-backup' : true)
        m.materializeAndroid.mockResolvedValue({ type: 'androidSpool', token: 'owned-copy' })
        m.materializeIOS.mockResolvedValue({ path: '/owned/copy.risudat', name: 'backup.bin', bytes: 42 })
        let finish!: (value: typeof result) => void
        m.legacy.mockImplementation(() => new Promise(resolve => { finish = resolve }))
        const restoring = restoreBackupFromNativeSource(source)
        await vi.waitFor(() => expect(m.legacy).toHaveBeenCalled())
        window.dispatchEvent(new Event('pagehide'))
        expect(m.legacy.mock.calls[0][2].signal.aborted).toBe(true)
        if (type === 'androidSeekable') expect(m.discard).toHaveBeenCalledWith('owned-copy')
        expect(m.invoke).toHaveBeenCalledWith('native_portable_source_discard', { source })
        finish(result)
        await restoring
    })
    it('cancels a stalled source probe without waiting for descriptor release', async () => {
        m.invoke.mockImplementation(() => new Promise(() => {}))
        const restoring = restoreBackupFromNativeSource({ type: 'androidSeekable', token: 'stalled-probe' })
        await vi.waitFor(() => expect(m.invoke).toHaveBeenCalledWith('native_backup_source_format', expect.anything()))
        m.controller.abort()
        await expect(restoring).rejects.toMatchObject({ name: 'AbortError' })
        expect(m.invoke).toHaveBeenCalledWith('native_portable_source_discard', { source: { type: 'androidSeekable', token: 'stalled-probe' } })
        expect(m.legacy).not.toHaveBeenCalled()
    })
    it('preserves the committed result and reports failed source cleanup', async () => {
        const committed = { ...result, warningCodes: [] as string[] }
        m.portable.mockResolvedValue(committed)
        m.invoke.mockImplementation(async command => command === 'native_backup_source_format' ? 'portable' : false)
        await expect(restoreBackupFromNativeSource({ type: 'iosScoped', token: 'source' }))
            .resolves.toEqual({ ...committed, warningCodes: ['cleanup-failed'] })
        expect(m.portable).toHaveBeenCalledTimes(1)
    })
    it('discards invalid seekable custody without materializing',async()=>{
        m.invoke.mockImplementation(async(command)=>{if(command==='native_backup_source_format')throw new Error('invalid-catalog');return true})
        await expect(restoreBackupFromNativeSource({type:'iosScoped',token:'synthetic-invalid'})).rejects.toThrow('invalid-catalog')
        expect(m.materializeIOS).not.toHaveBeenCalled()
        expect(m.invoke).toHaveBeenLastCalledWith('native_portable_source_discard',{source:{type:'iosScoped',token:'synthetic-invalid'}})
    })
    it.each(['portable', 'block-risu-save', 'local-backup'])(
        'uses detected %s independently of filename',
        async (format) => {
            m.invoke.mockResolvedValue(format)
            expect(
                await restoreBackupFromNativeSource({
                    type: 'desktopPath',
                    path: 'C:\\synthetic\\misleading.bin',
                }),
            ).toEqual(result)
            const selected =
                format === 'portable'
                    ? m.portable
                    : format === 'block-risu-save'
                      ? m.block
                      : m.legacy
            expect(selected).toHaveBeenCalledOnce()
            expect(m.manager).toHaveBeenCalledOnce()
            const options = selected.mock.calls[0][2]
            if (format === 'portable') {
                m.chooseRestore.mockResolvedValue({
                    library: true,
                    deviceSections: [],
                })
                await options.choosePortableSections({
                    libraryIncluded: true,
                    repairRequired: false,
                    deviceSections: [],
                })
                expect(m.chooseRestore).toHaveBeenCalledOnce()
            }
            await options.onNativeStatus?.({ state: 'succeeded', result })
            expect(m.hold).not.toHaveBeenCalled()
            expect(m.after).not.toHaveBeenCalled()
            await options.afterRefresh()
            expect(m.after).toHaveBeenCalledOnce()
            expect(m.hold).not.toHaveBeenCalled()
            expect(m.resume).not.toHaveBeenCalled()
        },
    )
    it.each(['compatibility-import-required', 'unsupported-format', 'corrupt-input'])('retains %s and releases an unclaimed source without a browser retry', async code => {
        const { NativeFileJobError } = await import('./nativeFileJobs')
        m.invoke.mockResolvedValue('local-backup')
        m.legacy.mockRejectedValue(new NativeFileJobError(code, 'Synthetic input failure'))
        await expect(restoreBackupFromNativeSource({type:'androidSpool',token:'synthetic'})).rejects.toMatchObject({code})
        expect(m.fallback).not.toHaveBeenCalled()
        expect(m.discard).toHaveBeenCalledWith('synthetic')
        expect(m.beginReplacement).not.toHaveBeenCalled()
    })

    it('native cancellation discards the unclaimed Android source', async () => {
        m.invoke.mockResolvedValue('local-backup')
        m.legacy.mockResolvedValueOnce(null)
        expect(
            await restoreBackupFromNativeSource({
                type: 'androidSpool',
                token: 'synthetic-token',
            }),
        ).toBeNull()
        expect(m.discard).toHaveBeenCalledWith('synthetic-token')
        expect(m.legacy).toHaveBeenCalledOnce()
    })
    it('passes lifecycle callbacks to the native guard without reserving server replacement', async () => {
        m.invoke.mockResolvedValue('block-risu-save')
        const beforeActivation=vi.fn()
        const onNativeStatus=vi.fn()
        await expect(restoreBackupFromNativeSource({type:'desktopPath',path:'synthetic.risudat'},{beforeActivation,onNativeStatus})).resolves.toEqual(result)
        expect(m.block.mock.calls[0][2]).toMatchObject({beforeActivation,onNativeStatus})
        expect(m.beginReplacement).not.toHaveBeenCalled()
        expect(m.releaseReplacement).not.toHaveBeenCalled()
    })
    it('forwards first-run consent to the native restore and section selection', async () => {
        m.invoke.mockResolvedValue('local-backup')
        expect(
            await restoreBackupFromNativeSource(
                { type: 'desktopPath', path: 'C:\\synthetic\\pocket.bin' },
                { firstRun: true },
            ),
        ).toEqual(result)
        expect(m.confirm).not.toHaveBeenCalled()
        expect(m.legacy).toHaveBeenCalledOnce()
        expect(m.legacy.mock.calls[0][2]).toMatchObject({firstRun:true})

        m.invoke.mockResolvedValue('portable')
        await restoreBackupFromNativeSource(
            { type: 'desktopPath', path: 'C:\\synthetic\\fresh.risunest' },
            { firstRun: true },
        )
        const preview = {
            libraryIncluded: true,
            repairRequired: false,
            deviceSections: [],
        }
        m.chooseRestore.mockResolvedValue({ library: true, deviceSections: [] })
        await m.portable.mock.calls[0][2].choosePortableSections(preview)
        expect(m.chooseRestore).toHaveBeenCalledExactlyOnceWith(preview, {
            firstRun: true,
        })
    })
    it('full native restoration delegates sync lifecycle to the shared guard', async () => {
        await restoreBackupFromNativeSource({
            type: 'desktopPath',
            path: 'C:\\synthetic\\device.risunest',
        })
        const options = m.portable.mock.calls[0][2]
        m.chooseRestore.mockResolvedValue({
            library: true,
            deviceSections: ['hypa','local-plugins','local-settings'],
        })
        await options.choosePortableSections({
            libraryIncluded: true,
            repairRequired: false,
            deviceSections: ['local-storage'],
        })
        await options.onNativeStatus?.({ state: 'succeeded', result })
        await options.afterRefresh()
        expect(m.hold).not.toHaveBeenCalled()
        expect(m.resume).not.toHaveBeenCalled()
    })
    it('rejects unsupported signatures before any restore', async () => {
        m.invoke.mockRejectedValue(new Error('unsupported-format'))
        await expect(
            restoreBackupFromNativeSource({
                type: 'androidSpool',
                token: 'synthetic-token',
            }),
        ).rejects.toThrow('unsupported-format')
        expect(m.discard).toHaveBeenCalledOnce()
        expect(m.portable).not.toHaveBeenCalled()
    })
    it('opens one common picker and returns normally when cancelled', async () => {
        m.open.mockResolvedValue(null)
        expect(await restoreBackupFromSystemPicker()).toBeNull()
        expect(m.open.mock.calls[0][0].filters[0].extensions).toEqual([
            'risunest',
            'bin',
            'risudat',
        ])
        expect(m.invoke).not.toHaveBeenCalled()
    })
    it('passes selected device sections and own extension to native full export', async () => {
        expect(await exportPortableBackupFromSystemPicker()).toEqual(result)
        expect(m.export.mock.calls[0][2]).toEqual({
            library: true,
            deviceSections: ['local-storage', 'localforage'],
        })
        expect(m.save.mock.calls[0][0].filters[0].extensions).toEqual([
            'risunest',
        ])
    })
    it('provides the account warning only when native source preservation requests confirmation', async () => {
        await exportPortableBackupFromSystemPicker()
        expect(m.confirm).not.toHaveBeenCalled()
        const { language } = await import('src/lang')
        m.confirm.mockResolvedValue(false)
        await expect(m.export.mock.calls[0][3].confirmSourcePreservation()).resolves.toBe(false)
        expect(m.confirm).toHaveBeenCalledExactlyOnceWith(language.risuNest.backup.sourcePreservationConfirm)
    })
    it('a cancelled section chooser never starts or picks a destination', async () => {
        m.chooseExport.mockResolvedValue(null)
        expect(await exportPortableBackupFromSystemPicker()).toBeNull()
        expect(m.export).not.toHaveBeenCalled()
        expect(m.save).not.toHaveBeenCalled()
    })
})
