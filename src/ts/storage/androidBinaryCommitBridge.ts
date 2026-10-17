export const ANDROID_BINARY_CHUNK_BYTES = 256 * 1024
const HEADER_BYTES = 40

let channelTail = Promise.resolve()

/** The native byte channel and persistence pool each admit one producer. */
export async function withAndroidByteChannel<T>(operation: () => Promise<T>): Promise<T> {
    const previous = channelTail
    let release!: () => void
    channelTail = new Promise<void>(resolve => { release = resolve })
    await previous
    try {
        return await operation()
    } finally {
        release()
    }
}

export interface AndroidBinaryCommitBridge {
    postMessage(packet: ArrayBuffer): void
    onmessage: ((event: { data: string }) => void) | null
}

/** Native registers this object only when both WebView features are supported. */
export function getAndroidBinaryCommitBridge(): AndroidBinaryCommitBridge | undefined {
    if (typeof window === 'undefined') return undefined
    const bridge = (window as Window & { RisuNestCommit?: AndroidBinaryCommitBridge })
        .RisuNestCommit
    return typeof bridge?.postMessage === 'function' ? bridge : undefined
}

export function binaryCommitSender(bridge: AndroidBinaryCommitBridge, id: string) {
    let pending: { resolve(offset: number): void; reject(error: Error): void } | undefined
    let timer: ReturnType<typeof setTimeout> | undefined
    const listener = ({ data }: { data: string }) => {
        if (!pending) return
        try {
            const result = JSON.parse(data)
            if (result.id !== id) return
            if (result.error) throw Object.assign(new Error('Android binary persistence chunk rejected'), { code: result.error })
            if (!Number.isSafeInteger(result.offset))
                throw new Error('Invalid binary acknowledgement')
            pending.resolve(result.offset)
        } catch (error) {
            pending.reject(
                error instanceof Error ? error : new Error('Invalid binary acknowledgement'),
            )
        }
    }
    bridge.onmessage = listener
    return {
        async append(offset: number, bytes: Uint8Array): Promise<number> {
            if (!/^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/.test(id))
                throw new Error('Invalid binary persistence ID')
            const packet = new Uint8Array(HEADER_BYTES + bytes.length)
            for (let i = 0; i < id.length; i++) packet[i] = id.charCodeAt(i)
            new DataView(packet.buffer).setUint32(36, offset, true)
            packet.set(bytes, HEADER_BYTES)
            for (let attempt = 0; ; attempt++) {
                try {
                    try {
                        return await new Promise<number>((resolve, reject) => {
                            pending = { resolve, reject }
                            timer = setTimeout(
                                () => reject(new Error('Android binary persistence timed out')),
                                10_000,
                            )
                            bridge.postMessage(packet.buffer)
                        })
                    } finally {
                        clearTimeout(timer)
                        pending = undefined
                    }
                } catch (error) {
                    // Busy is an explicit rejection before native admission, never an unknown ACK.
                    if (attempt >= 40 || (error as { code?: string })?.code !== 'busy') throw error
                    await new Promise(resolve => setTimeout(resolve, 250))
                }
            }
        },
        close() {
            clearTimeout(timer)
            pending = undefined
            if (bridge.onmessage === listener) bridge.onmessage = null
        },
    }
}
