import { MAX_NATIVE_REQUEST_BYTES, PayloadTooLargeError, prepareNativePersistenceValue, UnsaveableValueError, utf8ByteLength, type PayloadTooLargeKind } from './nativePersistenceValue'
import { invoke } from '@tauri-apps/api/core'
import { platform } from '@tauri-apps/plugin-os'
import type { AssetAlias, WorkingSetCommit } from './persistentDataStore'
import {
    getAndroidBinaryCommitBridge,
    type AndroidBinaryCommitBridge,
} from './androidBinaryCommitBridge'
import {
    ANDROID_LARGE_COMMIT_SIZE,
    sendAndroidCommit,
} from './androidCommitTransport'

export interface CommitEnvelope {
    commit: WorkingSetCommit
    assetAliases: AssetAlias[]
}
export const LARGE_COMMIT_BYTES = 1024 * 1024
/** An ordinary invoke carries one staged replace request up to this many bytes. */
export const STAGED_REQUEST_BYTES = 4 * 1024 * 1024
const PROBE_VISITS = 4096
const textEncoder = new TextEncoder()

/** A bounded routing hint, not another full JSON serialization on the UI thread. */
export function isLargeCommit(value: unknown, threshold = LARGE_COMMIT_BYTES): boolean {
    let visits = 0
    let size = 0
    const pending = [value]
    while (pending.length) {
        if (++visits > PROBE_VISITS) return true
        const item = pending.pop()
        if (typeof item === 'string') size += item.length
        else if (item && typeof item === 'object') {
            if (Array.isArray(item) && item.length + visits > PROBE_VISITS) return true
            const keys = Object.keys(item)
            if (keys.length + visits > PROBE_VISITS) return true
            for (const key of keys) {
                size += key.length
                pending.push((item as Record<string, unknown>)[key])
            }
        } else size += 8
        if (size >= threshold) return true
    }
    return false
}

interface SharedEvent {
    additionalData?: { kind?: string; requestId?: string; id?: string }
    getBuffer(): ArrayBuffer
}
export interface SharedWebview {
    addEventListener(type: 'sharedbufferreceived', listener: (event: SharedEvent) => void): void
    removeEventListener(type: 'sharedbufferreceived', listener: (event: SharedEvent) => void): void
    releaseBuffer(buffer: ArrayBuffer): void
}
export interface CommitTransportDependencies {
    windows(): boolean
    android?(): boolean
    linux?(): boolean
    ios?(): boolean
    macos?(): boolean
    androidBinary?(): AndroidBinaryCommitBridge | null
    invoke<T>(
        command: string,
        args?: Record<string, unknown> | Uint8Array,
    ): Promise<T>
    encode(input: CommitEnvelope): Promise<Uint8Array>
    shared(): SharedWebview | undefined
}

export class NativeCommitTransport {
    private rawUnavailable = false
    private pending: Promise<unknown> = Promise.resolve()
    constructor(private readonly dependencies: CommitTransportDependencies) {}

    commit(input: CommitEnvelope): Promise<{ revision: number }> {
        const run = this.pending.then(() => this.send(input))
        this.pending = run.catch(() => undefined)
        return run
    }

    /**
     * Sends one staged replace request. Every target refuses a request above
     * the native limit before sending it, and Android sends one above the
     * ordinary budget in chunks, in turn with commits.
     */
    async stage(command: string, kind: PayloadTooLargeKind, args: Record<string, unknown>): Promise<void> {
        const json = JSON.stringify(args)
        const head = `{"command":${JSON.stringify(command)},"args":`
        const byteLength = utf8ByteLength(head) + utf8ByteLength(json) + 1
        if (byteLength > MAX_NATIVE_REQUEST_BYTES) throw new PayloadTooLargeError(kind, byteLength)
        const deps = this.dependencies
        if (!(deps.android?.() ?? false) || byteLength <= STAGED_REQUEST_BYTES) {
            await deps.invoke(command, args)
            return
        }
        const body = textEncoder.encode(`${head}${json}}`)
        const run = this.pending.then(() => sendAndroidCommit<void>(
            body,
            deps.invoke,
            deps.androidBinary ? deps.androidBinary() : getAndroidBinaryCommitBridge(),
            'pds_replace_android_finish',
        ))
        this.pending = run.catch(() => undefined)
        await run
    }

    private async sendRaw(bytes: Uint8Array): Promise<{ revision: number }> {
        const json = () => this.dependencies.invoke<{ revision: number }>(
            'pds_commit', JSON.parse(new TextDecoder().decode(bytes)),
        )
        if (this.rawUnavailable) return json()
        try {
            return await this.dependencies.invoke('pds_commit_raw', bytes)
        } catch (error) {
            let value = error
            if (typeof value === 'string') {
                try { value = JSON.parse(value) } catch { /* Not a native typed rejection. */ }
            }
            if (!value || typeof value !== 'object' || (value as { code?: string }).code !== 'raw-body-unavailable') throw error
            // The endpoint rejected the body before decoding or entering the store.
            this.rawUnavailable = true
            return json()
        }
    }
    private async send(input: CommitEnvelope): Promise<{ revision: number }> {
        const deps = this.dependencies
        const android = deps.android?.() ?? false
        const rawPlatform =
            (deps.linux?.() ?? false) || (deps.macos?.() ?? false) || (deps.ios?.() ?? false)
        if (
            (!android && !rawPlatform && !deps.windows()) ||
            !isLargeCommit(
                input,
                android ? ANDROID_LARGE_COMMIT_SIZE : LARGE_COMMIT_BYTES,
            )
        )
            return deps.invoke('pds_commit', { ...prepareNativePersistenceValue(input) })
        let bytes: Uint8Array
        try {
            bytes = await deps.encode(input)
        } catch (error) {
            // Callable hooks cannot be structured-cloned. No native save has started yet.
            if (error instanceof DOMException && error.name === 'DataCloneError') {
                return deps.invoke('pds_commit', { ...prepareNativePersistenceValue(input) })
            }
            throw error
        }
        // The Android transport assembles at most this much, and every target
        // keeps the same limit so a save that works on one works on all.
        if (bytes.byteLength > MAX_NATIVE_REQUEST_BYTES) throw new PayloadTooLargeError('commit', bytes.byteLength)
        if (android) {
            return sendAndroidCommit(
                bytes,
                deps.invoke,
                deps.androidBinary ? deps.androidBinary() : getAndroidBinaryCommitBridge(),
            )
        }
        if (rawPlatform) return this.sendRaw(bytes)
        const webview = deps.shared()
        if (!webview) return this.sendRaw(bytes)
        const requestId = crypto.randomUUID()
        let buffer: ArrayBuffer | undefined
        let nativeId: string | undefined
        let receivedId: string | undefined
        let receive!: () => void
        const received = new Promise<void>((resolve) => {
            receive = resolve
        })
        const listener = (event: SharedEvent) => {
            if (
                event.additionalData?.kind !== 'pds-commit' ||
                event.additionalData.requestId !== requestId
            )
                return
            if (buffer) return
            receivedId = event.additionalData.id
            buffer = event.getBuffer()
            receive()
        }
        webview.addEventListener('sharedbufferreceived', listener)
        let timer: ReturnType<typeof setTimeout> | undefined
        try {
            const opened = await deps.invoke<{ id: string; capacity: number } | null>(
                'pds_commit_shared_open',
                { requestId, totalBytes: bytes.byteLength },
            )
            if (!opened) return this.sendRaw(bytes)
            nativeId = opened.id
            await Promise.race([
                received,
                new Promise<never>((_, reject) => {
                    timer = setTimeout(
                        () => reject(new Error('Timed out receiving persistence shared buffer')),
                        10_000,
                    )
                }),
            ])
            if (
                receivedId !== nativeId ||
                !buffer ||
                !Number.isSafeInteger(opened.capacity) ||
                opened.capacity <= 0 ||
                opened.capacity !== buffer.byteLength
            ) {
                throw new Error('Invalid persistence shared buffer')
            }
            const shared = new Uint8Array(buffer)
            for (let offset = 0; offset < bytes.byteLength;) {
                const length = Math.min(shared.length, bytes.byteLength - offset)
                shared.set(bytes.subarray(offset, offset + length))
                const ack = await deps.invoke<number>('pds_commit_shared_chunk', {
                    id: nativeId,
                    offset,
                    length,
                })
                if (ack !== offset + length)
                    throw new Error('Invalid persistence chunk acknowledgement')
                offset = ack
            }
            // From this point an error is potentially a committed save. Never replay it.
            return await deps.invoke('pds_commit_shared_finish', { id: nativeId })
        } finally {
            if (timer !== undefined) clearTimeout(timer)
            try {
                webview.removeEventListener('sharedbufferreceived', listener)
            } catch (cleanupError) {
                console.error('Persistence shared commit listener cleanup failed', cleanupError)
            }
            try {
                if (buffer) webview.releaseBuffer(buffer)
            } catch (cleanupError) {
                console.error('Persistence shared commit buffer cleanup failed', cleanupError)
            } finally {
                try {
                    await deps.invoke('pds_commit_shared_cancel', { requestId })
                } catch (cleanupError) {
                    console.error('Persistence shared commit cleanup failed', cleanupError)
                }
            }
        }
    }
}

let encoder: Worker | undefined
let encoderIdle: ReturnType<typeof setTimeout> | undefined
export function encodeNativeCommit(input: CommitEnvelope): Promise<Uint8Array> {
    if (encoderIdle) clearTimeout(encoderIdle)
    encoder ??= new Worker(new URL('./nativeCommitEncoder.worker.ts', import.meta.url), {
        type: 'module',
    })
    const worker = encoder
    return new Promise((resolve, reject) => {
        const cleanup = () => {
            worker.onmessage = null
            worker.onerror = null
            encoderIdle = setTimeout(() => {
                worker.terminate()
                if (encoder === worker) encoder = undefined
            }, 30_000)
        }
        worker.onmessage = ({ data }: MessageEvent<{ bytes?: Uint8Array; error?: string; code?: string; area?: string; reason?: string; recordId?: string }>) => {
            cleanup()
            if (data.bytes) resolve(data.bytes)
            else if (data.code === 'unsaveable-value') reject(new UnsaveableValueError(data.area ?? 'persistent data', data.reason ?? 'unsupported value', data.recordId))
            else reject(new Error(data.error ?? 'Persistence encoding failed'))
        }
        worker.onerror = () => {
            cleanup()
            worker.terminate()
            encoder = undefined
            reject(new Error('Persistence encoder failed'))
        }
        try {
            worker.postMessage(input)
        } catch (error) {
            cleanup()
            reject(error)
        }
    })
}

export const nativeCommitTransport = new NativeCommitTransport({
    ios: () =>
        Boolean(
            (window as Window & { __TAURI_INTERNALS__?: unknown })
                .__TAURI_INTERNALS__,
        ) && platform() === 'ios',
    macos: () =>
        Boolean(
            (window as Window & { __TAURI_INTERNALS__?: unknown })
                .__TAURI_INTERNALS__,
        ) && platform() === 'macos',
    linux: () =>
        Boolean(
            (window as Window & { __TAURI_INTERNALS__?: unknown })
                .__TAURI_INTERNALS__,
        ) && platform() === 'linux',
    android: () =>
        Boolean(
            (window as Window & { __TAURI_INTERNALS__?: unknown })
                .__TAURI_INTERNALS__,
        ) && platform() === 'android',
    windows: () =>
        Boolean(
            (window as Window & { __TAURI_INTERNALS__?: unknown })
                .__TAURI_INTERNALS__,
        ) && platform() === 'windows',
    invoke,
    encode: encodeNativeCommit,
    shared: () =>
        (window as Window & { chrome?: { webview?: SharedWebview } }).chrome
            ?.webview,
})
