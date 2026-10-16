import { afterEach, describe, expect, it, vi } from 'vitest'
import {
    copyNativeExportToAndroidSaf,
    reserveAndroidSafDestination,
    withAndroidSafDestination,
    pickAndroidBackupSource,
    type AndroidSafJavascriptBridge,
} from './androidSafBridge'
import { portableAndroidPublicationDependencies, type PendingPortableExport } from './deviceBackup/job'

const {invokeNative} = vi.hoisted(() => ({invokeNative: vi.fn()}))
vi.mock('@tauri-apps/api/core', () => ({invoke: invokeNative}))

const requestId = '11111111-1111-4111-8111-111111111111'
const exportId = '22222222-2222-4222-8222-222222222222'

function fixture(bridge: AndroidSafJavascriptBridge) {
    const target = new EventTarget()
    const remove = vi.fn((name: string, listener: (event: Event) => void) => target.removeEventListener(name, listener))
    return {
        dependencies: {
            bridge,
            createRequestId: () => requestId,
            addEventListener: (name: string, listener: (event: Event) => void) => target.addEventListener(name, listener),
            removeEventListener: remove,
        },
        remove,
        emit: (name: string, detail: unknown) => target.dispatchEvent(new CustomEvent(name, { detail })),
    }
}

const request = { sourcePath: '/synthetic/owned/archive.bin', suggestedName: 'backup.bin' }
const terminal = { requestId, exportId, state: 'succeeded' as const, bytes: 3, warningCodes: [] }

afterEach(() => { delete window.RisuSafBridge; invokeNative.mockReset() })

describe('asynchronous Android file ownership', () => {
    it.each(['failed', 'cancelled', 'succeeded'] as const)('keeps the %s operation outcome and cleanup warnings', async (outcome) => {
        let selected = false
        const {dependencies, emit} = fixture({copyExport: vi.fn(), copyReservedExport: vi.fn(),
            reserveExport: vi.fn(), bindExport: async () => true, releaseExport: async () => true,
            getExportStatus: async () => selected ? JSON.stringify({...terminal,
                state: 'cancelled', warningCodes: ['partial-destination-may-remain']}) : null,
        })
        const picking = reserveAndroidSafDestination('synthetic.bin', 'download', undefined, dependencies)
        await Promise.resolve()
        selected = true
        emit('risunest-android-saf-reserved', {requestId})
        const reservation = await picking
        const failure = outcome === 'cancelled' ? new DOMException('cancelled', 'AbortError') : new Error('producer failed')
        const output = withAndroidSafDestination(reservation, async () => {
            if (outcome !== 'succeeded') throw failure
            return true
        })
        if (outcome === 'succeeded') {
            await expect(output).rejects.toMatchObject({code: 'destination-cleanup-failed'})
        } else {
            await expect(output).rejects.toBe(failure)
            expect(failure).toMatchObject({warningCodes: ['partial-destination-may-remain']})
        }
    })

    it.each(['download', 'legacyBackup'] as const)('recovers orphaned %s receipts before opening another destination', async (sourceKind) => {
        for (const state of ['succeeded', 'failed', 'cancelled'] as const) {
            let saved: string | null = JSON.stringify({...terminal, requestId: exportId, sourceKind, state})
            const {dependencies, emit} = fixture({copyExport: vi.fn(), copyReservedExport: vi.fn(),
                bindExport: async () => true, releaseExport: async () => false,
                getExportStatus: async () => saved,
                acknowledgeExport: async (id) => {
                    if (id !== exportId) return false
                    saved = null
                    return true
                },
                reserveExport: () => {
                    if (saved) throw new Error('destination-busy')
                    emit('risunest-android-saf-reserved', {requestId})
                }})
            const reservation = await reserveAndroidSafDestination('next.bin', 'download', undefined, dependencies)
            expect(reservation.requestId).toBe(requestId)
            expect(saved).toBeNull()
            await reservation.release()
        }
    })

    it('retains an orphaned receipt when acknowledgement fails and retries its exact identity', async () => {
        let acknowledged = false
        let saved: string | null = JSON.stringify({...terminal, requestId: exportId, sourceKind: 'download'})
        const {dependencies, emit} = fixture({copyExport: vi.fn(), copyReservedExport: vi.fn(),
            bindExport: async () => true, releaseExport: async () => false, getExportStatus: async () => saved,
            acknowledgeExport: async (id) => {
                if (!acknowledged || id !== exportId) return false
                saved = null
                return true
            },
            reserveExport: () => { emit('risunest-android-saf-reserved', {requestId}) }})
        await expect(reserveAndroidSafDestination('next.bin', 'download', undefined, dependencies))
            .rejects.toMatchObject({code: 'acknowledgement-failed', requestId: exportId})
        expect(saved).not.toBeNull()
        acknowledged = true
        const reservation = await reserveAndroidSafDestination('next.bin', 'download', undefined, dependencies)
        expect(saved).toBeNull()
        await reservation.release()
    })

    it.each(['risuSave'] as const)('preserves %s publication owners while reserving', async (sourceKind) => {
        const saved = JSON.stringify({...terminal, requestId: exportId, sourceKind})
        const acknowledgeExport = vi.fn(async () => true)
        const {dependencies} = fixture({copyExport: vi.fn(), copyReservedExport: vi.fn(),
            bindExport: async () => true, releaseExport: async () => false, getExportStatus: async () => saved,
            acknowledgeExport, reserveExport: () => {throw new Error('destination-busy')}})
        await expect(reserveAndroidSafDestination('next.bin', 'download', undefined, dependencies)).rejects.toThrow('destination-busy')
        expect(acknowledgeExport).not.toHaveBeenCalled()
    })

    it('retries the screenshot owner on a later export after asynchronous cancellation settles', async () => {
        let publishing = true
        let saved: string | null = JSON.stringify({...terminal, requestId: exportId, exportId: requestId,
            sourceKind: 'screenshot', state: 'cancelled', copyStarted: false})
        const acknowledgeExport = vi.fn(async (id: string) => {
            if (id !== exportId || publishing) return false
            saved = null
            return true
        })
        invokeNative.mockImplementation(async (command, args) => {
            expect(args.jobId).toBe(requestId)
            if (command === 'native_file_job_screenshot_output_cancel') return 'requested'
            if (command === 'native_file_job_screenshot_output_release' && publishing) throw {code: 'invalid-state'}
        })
        const reserveExport = vi.fn(() => {
            if (saved) throw new Error('destination-busy')
            emit('risunest-android-saf-reserved', {requestId})
        })
        const {dependencies, emit} = fixture({copyExport: vi.fn(), copyReservedExport: vi.fn(),
            bindExport: async () => true, releaseExport: async () => false,
            getExportStatus: () => saved, acknowledgeExport, reserveExport})
        await expect(reserveAndroidSafDestination('next.bin', 'download', undefined, dependencies))
            .rejects.toMatchObject({code: 'invalid-state'})
        expect(saved).not.toBeNull()
        expect(acknowledgeExport).not.toHaveBeenCalled()
        expect(reserveExport).not.toHaveBeenCalled()
        publishing = false
        const next = await reserveAndroidSafDestination('next.bin', 'download', undefined, dependencies)
        expect(saved).toBeNull()
        expect(acknowledgeExport).toHaveBeenCalledExactlyOnceWith(exportId)
        expect(next.requestId).toBe(requestId)
        await next.release()
    })

    it('does not consume a receipt owned by an active reservation', async () => {
        const first = fixture({copyExport: vi.fn(), copyReservedExport: vi.fn(), reserveExport: vi.fn(),
            bindExport: async () => true, releaseExport: async () => false})
        const picking = reserveAndroidSafDestination('active.bin', 'download', undefined, first.dependencies)
        first.emit('risunest-android-saf-reserved', {requestId})
        const active = await picking
        const acknowledgeExport = vi.fn(async () => true)
        const next = fixture({copyExport: vi.fn(), copyReservedExport: vi.fn(),
            bindExport: async () => true, releaseExport: async () => false,
            getExportStatus: () => JSON.stringify({...terminal, sourceKind: 'download'}),
            acknowledgeExport, reserveExport: () => {throw new Error('destination-busy')}})
        next.dependencies.createRequestId = () => exportId
        await expect(reserveAndroidSafDestination('next.bin', 'download', undefined, next.dependencies)).rejects.toThrow('destination-busy')
        expect(acknowledgeExport).not.toHaveBeenCalled()
        await active.release()
    })

    it('returns no destination until selection and binds the selected request to its native job', async () => {
        const copyReservedExport = vi.fn()
        const bindExport = vi.fn(async () => true)
        const {dependencies, emit} = fixture({copyExport: vi.fn(), copyReservedExport,
            reserveExport: vi.fn(), bindExport, releaseExport: async () => true})
        let selected = false
        const picking = reserveAndroidSafDestination('synthetic.risunest', 'risuSave', undefined, dependencies)
        void picking.then(() => {selected = true})
        emit('risunest-android-saf-reserved', {requestId: 'different-request'})
        await Promise.resolve()
        expect(selected).toBe(false)
        emit('risunest-android-saf-reserved', {requestId})
        const reservation = await picking
        await reservation.bind('aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa')
        const copied = copyNativeExportToAndroidSaf({...request, reservation}, dependencies)
        await vi.waitFor(() => expect(copyReservedExport).toHaveBeenCalled())
        expect(copyReservedExport).toHaveBeenCalledWith(requestId, request.sourcePath, 'aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa')
        emit('risu-android-saf-destination', terminal)
        await expect(copied).resolves.toMatchObject({bytes: 3, requestId})
        await reservation.release()
    })

    it('does not expose a late selected destination after cancellation and releases only its own reservation', async () => {
        const controller = new AbortController()
        const releaseExport = vi.fn(async () => true)
        const bindExport = vi.fn(async () => true)
        const {dependencies, emit} = fixture({copyExport: vi.fn(), copyReservedExport: vi.fn(),
            reserveExport: vi.fn(), bindExport, releaseExport, cancelExport: async () => true})
        const picking = reserveAndroidSafDestination('synthetic.zip', 'screenshot', controller.signal, dependencies)
        const rejected = expect(picking).rejects.toMatchObject({name: 'AbortError'})
        controller.abort()
        emit('risunest-android-saf-reserved', {requestId})
        await rejected
        expect(releaseExport).toHaveBeenCalledWith(requestId)
        expect(bindExport).not.toHaveBeenCalled()
    })

    it('refuses a changed job identity before invoking reserved copy', async () => {
        const copyReservedExport = vi.fn()
        const {dependencies, emit} = fixture({copyExport: vi.fn(), copyReservedExport,
            reserveExport: vi.fn(), bindExport: async () => false, releaseExport: async () => true})
        const picking = reserveAndroidSafDestination('synthetic.zip', 'screenshot', undefined, dependencies)
        emit('risunest-android-saf-reserved', {requestId})
        const reservation = await picking
        await expect(reservation.bind(exportId)).rejects.toMatchObject({code: 'invalid-destination'})
        expect(reservation.jobId).toBeUndefined()
        expect(copyReservedExport).not.toHaveBeenCalled()
        await reservation.release()
    })

    it('awaits a refused acknowledgement and processes duplicate terminals only once', async () => {
        let finish!: (value: boolean) => void
        const acknowledgeExport = vi.fn(() => new Promise<boolean>((resolve) => { finish = resolve }))
        const { dependencies, emit } = fixture({ copyExport: vi.fn(), acknowledgeExport })
        const output = copyNativeExportToAndroidSaf(request, dependencies)
        const rejected = expect(output).rejects.toMatchObject({ code: 'acknowledgement-failed', requestId })
        emit('risu-android-saf-destination', terminal)
        emit('risu-android-saf-destination', terminal)
        await vi.waitFor(() => expect(acknowledgeExport).toHaveBeenCalledOnce())
        finish(false)
        await rejected
    })

    it('retains the copy listener after an uncertain cancellation reply', async () => {
        const controller = new AbortController()
        const cancelExport = vi.fn(async () => { throw new Error('reply lost') })
        const { dependencies, emit, remove } = fixture({ copyExport: vi.fn(), cancelExport })
        let settled = false
        const output = copyNativeExportToAndroidSaf({ ...request, signal: controller.signal }, dependencies)
        void output.then(() => { settled = true })
        controller.abort()
        await vi.waitFor(() => expect(cancelExport).toHaveBeenCalledOnce())
        expect(settled).toBe(false)
        expect(remove).not.toHaveBeenCalled()
        emit('risu-android-saf-destination', terminal)
        await expect(output).resolves.toMatchObject({ bytes: 3 })
        expect(remove).toHaveBeenCalled()
    })

    it('cleans the original cancelled source once after its terminal arrives', async () => {
        const controller = new AbortController()
        let finish!: (value: boolean) => void
        const discardSource = vi.fn(() => new Promise<boolean>((resolve) => { finish = resolve }))
        const { dependencies, emit, remove } = fixture({
            copyExport: vi.fn(), pickBackupSource: vi.fn(), discardSource,
            cancelSource: async () => { throw new Error('reply lost') },
        })
        const source = pickAndroidBackupSource({ signal: controller.signal }, dependencies)
        const rejected = expect(source).rejects.toMatchObject({ name: 'AbortError' })
        controller.abort()
        const batch = { requestId, ready: [{ token: exportId, displayName: 'backup.bin', bytes: 3 }], failures: [] }
        emit('risu-android-backup-source-picked', batch)
        emit('risu-android-backup-source-picked', batch)
        await vi.waitFor(() => expect(discardSource).toHaveBeenCalledOnce())
        expect(remove).not.toHaveBeenCalled()
        finish(true)
        await rejected
        expect(remove).toHaveBeenCalled()
    })

    it('does not lose completion delivered during an asynchronous recovery lookup', async () => {
        let finish!: (value: string | null) => void
        const copyExport = vi.fn()
        window.RisuSafBridge = {
            copyExport,
            getExportStatus: () => new Promise((resolve) => { finish = resolve }),
            getExportSourceId: async () => null,
        }
        const intent: PendingPortableExport = {
            schema: 'risunest.portable-export-intent/v1',
            jobId: 'synthetic-job', publication: 'android-saf', phase: 'publishing',
            requestId, suggestedName: 'backup.risunest',
        }
        const result = {
            revision: 1, sourceBytes: 3, sourceFingerprintKind: 'whole-file-sha256' as const, sourceSha256: 'a'.repeat(64),
            characterCount: 1, presetCount: 1, warningCodes: [],
            handoffPath: `/synthetic/owned/${exportId}/backup.risunest`,
        }
        const output = portableAndroidPublicationDependencies().resumeAndroid(intent, result)
        window.dispatchEvent(new CustomEvent('risu-android-saf-destination', { detail: terminal }))
        finish(null)
        await expect(output).resolves.toEqual(terminal)
        expect(copyExport).not.toHaveBeenCalled()
    })
})
