import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'

const mocks = vi.hoisted(() => ({
    android: true, ios: false, invoke: vi.fn(), open: vi.fn(), pick: vi.fn(), discard: vi.fn(),
}))
vi.mock('../platform', () => ({
    get isTauriAndroid() { return mocks.android },
    get isTauriIOS() { return mocks.ios },
    isTauriMobile: true,
}))
vi.mock('@tauri-apps/api/core', () => ({ invoke: mocks.invoke }))
vi.mock('@tauri-apps/plugin-fs', () => ({ open: mocks.open }))
import { selectNativeFileData, selectNativeFiles, type NativeSelectedFile } from './nativeFileSelection'
import { selectDomFiles } from './fileSelection'
import { isLibraryFileOperationReserved } from './libraryFileOperation'

const metadata = (name = 'synthetic.risup', bytes = 4): NativeSelectedFile => ({
    path: '/owned/selection/source', name, bytes, mimeType: 'application/octet-stream',
})
const handles: { close: ReturnType<typeof vi.fn>; read: ReturnType<typeof vi.fn> }[] = []

function readable(bytes = new Uint8Array([0, 128, 255, 10]), maxRead = Infinity) {
    let position = 0
    const handle = {
        read: vi.fn(async (target: Uint8Array) => {
            const chunk = bytes.subarray(position, position + Math.min(target.length, maxRead))
            target.set(chunk)
            position += chunk.length
            return chunk.length || null
        }),
        close: vi.fn(async () => {}),
    }
    handles.push(handle)
    return handle
}

beforeEach(() => {
    vi.clearAllMocks()
    mocks.android = true
    mocks.ios = false
    handles.length = 0
    window.RisuNativeFileSelection = { pick: mocks.pick, discard: mocks.discard }
    mocks.pick.mockResolvedValue({ cancelled: false, files: [metadata()] })
    mocks.discard.mockResolvedValue(undefined)
    mocks.open.mockImplementation(async () => readable())
})
afterEach(() => { delete window.RisuNativeFileSelection })

describe('native mobile file selection', () => {
    it('reads exact binary bytes with short native reads and releases the handle and source', async () => {
        mocks.open.mockImplementation(async () => readable(undefined, 1))
        const [file] = await selectNativeFileData({ accept: '.json,.risup' })
        expect(file.name).toBe('synthetic.risup')
        expect([...file.data]).toEqual([0, 128, 255, 10])
        expect(mocks.open).toHaveBeenCalledWith('/owned/selection/source', { read: true })
        expect(handles[0].close).toHaveBeenCalledOnce()
        expect(mocks.discard).toHaveBeenCalledWith('/owned/selection/source')
        expect(isLibraryFileOperationReserved()).toBe(false)
    })

    it('bounds native IPC reads for a file larger than one chunk', async () => {
        const bytes = new Uint8Array(600_000).fill(193)
        mocks.pick.mockResolvedValue({ cancelled: false, files: [metadata('large.bin', bytes.length)] })
        mocks.open.mockImplementation(async () => readable(bytes))
        const [file] = await selectNativeFileData()
        expect(file.data).toEqual(bytes)
        expect(handles[0].read.mock.calls.every(([target]) => target.length <= 256 * 1024)).toBe(true)
    })

    it('uses the iOS native picker and cleanup, preserving multiple selection order and names', async () => {
        mocks.android = false
        mocks.ios = true
        mocks.invoke.mockResolvedValueOnce({ cancelled: false, files: [metadata('첫째.RISUP'), metadata('second.json')] })
            .mockResolvedValue(undefined)
        const files = await selectNativeFileData({ multiple: true, accept: '.risup,.json' })
        expect(files.map(file => file.name)).toEqual(['첫째.RISUP', 'second.json'])
        expect(mocks.invoke).toHaveBeenCalledWith('plugin:ios-native|pick_files', { multiple: true })
        expect(mocks.invoke.mock.calls.filter(([command]) => command === 'plugin:ios-native|discard_file')).toHaveLength(2)
    })

    it('routes the DOM-compatible selector through native reads on mobile without mounting an input', async () => {
        const files = await selectDomFiles({ accept: '.risup' })
        expect(document.querySelector('input[type=file]')).toBeNull()
        expect(files[0].name).toBe('synthetic.risup')
        expect(new Uint8Array(await files[0].arrayBuffer())).toEqual(new Uint8Array([0, 128, 255, 10]))
    })

    it('filters metadata before reading and still releases rejected selections', async () => {
        expect(await selectNativeFiles({ accept: '.png' })).toEqual([])
        expect(mocks.open).not.toHaveBeenCalled()
        expect(mocks.discard).toHaveBeenCalledOnce()
    })

    it('returns cancellation without attempting a read', async () => {
        mocks.pick.mockResolvedValue({ cancelled: true })
        expect(await selectNativeFileData()).toEqual([])
        expect(mocks.open).not.toHaveBeenCalled()
    })

    it('does not turn a picker failure into cancellation', async () => {
        mocks.pick.mockRejectedValue(new Error('SecurityException: document access denied'))
        await expect(selectNativeFileData()).rejects.toThrow('SecurityException: document access denied')
        expect(isLibraryFileOperationReserved()).toBe(false)
    })

    it('rejects concurrent pickers and retains admission until the native picker returns', async () => {
        let finish!: (value: { cancelled: boolean }) => void
        mocks.pick.mockImplementation(() => new Promise(resolve => { finish = resolve }))
        const first = selectNativeFileData()
        await expect(selectNativeFileData()).rejects.toThrow('already open')
        expect(isLibraryFileOperationReserved()).toBe(true)
        finish({ cancelled: true })
        await first
        expect(isLibraryFileOperationReserved()).toBe(false)
    })

    it('discards a result cancelled while the native picker was open', async () => {
        const controller = new AbortController()
        mocks.pick.mockImplementation(async () => {
            controller.abort()
            return { cancelled: false, files: [metadata()] }
        })
        expect(await selectNativeFileData({ signal: controller.signal })).toEqual([])
        expect(mocks.open).not.toHaveBeenCalled()
        expect(mocks.discard).toHaveBeenCalledOnce()
    })

    it('reports truncated data and closes the handle', async () => {
        mocks.open.mockImplementation(async () => readable(new Uint8Array([0])))
        await expect(selectNativeFileData()).rejects.toThrow('ended before all bytes')
        expect(handles[0].close).toHaveBeenCalledOnce()
        expect(mocks.discard).toHaveBeenCalledOnce()
    })

    it('releases every source after partial multi-file failure, preserving the read error', async () => {
        mocks.pick.mockResolvedValue({ cancelled: false, files: [metadata('first'), { ...metadata('second'), path: '/owned/second/source' }] })
        mocks.open.mockResolvedValueOnce(readable()).mockRejectedValueOnce(new Error('native read denied'))
        await expect(selectNativeFileData({ multiple: true })).rejects.toThrow('native read denied')
        expect(mocks.discard).toHaveBeenCalledTimes(2)
        expect(handles[0].close).toHaveBeenCalledOnce()
    })

    it('retains both read and cleanup errors', async () => {
        mocks.pick.mockResolvedValue({ cancelled: false, files: [metadata(), { ...metadata(), path: '/owned/second/source' }] })
        mocks.open.mockRejectedValue(new Error('read denied'))
        mocks.discard.mockRejectedValueOnce(new Error('cleanup denied')).mockResolvedValueOnce(undefined)
        await expect(selectNativeFileData()).rejects.toThrow('Error: read denied\nError: cleanup denied')
        expect(mocks.discard).toHaveBeenCalledTimes(2)
        expect(isLibraryFileOperationReserved()).toBe(false)
    })

    it('releases more files than the Android control transport permits in flight', async () => {
        const { createAndroidControlClient } = await import('../androidNativeControl')
        const port: import('../androidNativeControl').AndroidControlMessagePort = {
            postMessage: message => {
                const { id } = JSON.parse(message)
                queueMicrotask(() => port.onmessage?.({ data: JSON.stringify({ id, result: null }) }))
            },
        }
        const client = createAndroidControlClient(port)
        mocks.pick.mockResolvedValue({ cancelled: false, files: Array.from({ length: 40 }, (_, index) => ({
            ...metadata(), path: `/owned/${index}/source`,
        })) })
        mocks.discard.mockImplementation(path => client.request('files.discard', path))
        await expect(selectNativeFileData({ multiple: true, accept: '.txt' })).resolves.toEqual([])
        expect(mocks.discard).toHaveBeenCalledTimes(40)
        expect(isLibraryFileOperationReserved()).toBe(false)
    })
})
