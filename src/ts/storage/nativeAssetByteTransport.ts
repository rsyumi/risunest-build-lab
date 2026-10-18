import type { InvokeArgs, InvokeOptions } from '@tauri-apps/api/core'
import { ANDROID_BINARY_CHUNK_BYTES, binaryCommitSender, getAndroidBinaryCommitBridge, withAndroidByteChannel } from './androidBinaryCommitBridge'

export type NativeByteInvoke = (command: string, args?: InvokeArgs, options?: InvokeOptions) => Promise<unknown>
export const NATIVE_BINARY_PART_BYTES = 256 * 1024
const rawSupport = new WeakMap<NativeByteInvoke, Promise<boolean>>()

export async function nativeAssetByteSender(invoke: NativeByteInvoke) {
    const bridge = getAndroidBinaryCommitBridge()
    let raw = false
    if (!bridge) {
        let support = rawSupport.get(invoke)
        if (!support) {
            support = invoke('native_asset_bytes_probe', new Uint8Array()).then(value => {
                if (typeof value !== 'boolean') throw new TypeError('Invalid native byte capability response')
                return value
            }).catch(error => {
                rawSupport.delete(invoke)
                throw error
            })
            rawSupport.set(invoke, support)
        }
        raw = await support
    }
    return {
        binary: Boolean(bridge || raw),
        capacity: bridge ? ANDROID_BINARY_CHUNK_BYTES : raw ? NATIVE_BINARY_PART_BYTES : 64 * 1024,
        async append(command: string, uploadId: string, offset: number, data: Uint8Array): Promise<unknown> {
            if (bridge) {
                // Yield between parts so persistence and device storage can acquire the channel.
                return withAndroidByteChannel(async () => {
                    const sender = binaryCommitSender(bridge, uploadId)
                    try { return await sender.append(offset, data) } finally { sender.close() }
                })
            }
            if (!raw) return invoke(command, { uploadId, offset, data: Array.from(data) })
            try {
                return await invoke(`${command}_raw`, data, { headers: {
                    'x-risunest-upload-id': uploadId,
                    'x-risunest-offset': String(offset),
                } })
            } catch (error) {
                if ((error as { code?: string })?.code === 'raw-body-unavailable') {
                    rawSupport.set(invoke, Promise.resolve(false))
                }
                // Tauri may have resent after a failed response. Never replay this upload.
                throw error
            }
        },
    }
}
