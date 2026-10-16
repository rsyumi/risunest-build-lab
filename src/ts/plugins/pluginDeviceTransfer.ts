import { invoke } from '@tauri-apps/api/core'
import { isTauriAndroid } from '../platform'
import { ANDROID_LARGE_COMMIT_SIZE, sendAndroidCommit } from '../storage/androidCommitTransport'
import type { PluginDeviceMutation, PluginDeviceSpace, PluginDeviceObservation } from './pluginDeviceKeyspace'

export const PLUGIN_DEVICE_VALUE_BYTES = 512 * 1024 * 1024
export const PLUGIN_DEVICE_PART_BYTES = 8 * 1024 * 1024
const REQUEST_BYTES = 64 * 1024 * 1024
const encoder = new TextEncoder()

function characterSize(text: string, index: number): { units: number; bytes: number; encoded: number } {
    const code = text.charCodeAt(index)
    if (code < 0x80) {
        return { units: 1, bytes: 1, encoded: code === 34 || code === 92 || code === 8 || code === 9 || code === 10 || code === 12 || code === 13 ? 2 : code < 32 ? 6 : 1 }
    }
    if (code < 0x800) return { units: 1, bytes: 2, encoded: 2 }
    if (code >= 0xd800 && code <= 0xdbff) {
        const low = text.charCodeAt(index + 1)
        if (!(low >= 0xdc00 && low <= 0xdfff)) throw new Error('Plugin device value contains an unpaired surrogate')
        return { units: 2, bytes: 4, encoded: 4 }
    }
    if (code >= 0xdc00 && code <= 0xdfff) throw new Error('Plugin device value contains an unpaired surrogate')
    return { units: 1, bytes: 3, encoded: 3 }
}

export function pluginDeviceValueSize(value: string, limit = PLUGIN_DEVICE_VALUE_BYTES): number {
    let size = 2
    for (let index = 0; index < value.length;) {
        const character = characterSize(value, index)
        size += character.encoded
        if (size > limit) throw new Error('Plugin device value is too large')
        index += character.units
    }
    if (size > limit) throw new Error('Plugin device value is too large')
    return size
}

export function* pluginDeviceParts(value: string, limit = PLUGIN_DEVICE_PART_BYTES): Generator<{ part: string; byteLength: number }> {
    let start = 0, index = 0, encoded = 2, byteLength = 0
    while (index < value.length) {
        const character = characterSize(value, index)
        if (character.encoded + 2 > limit) throw new Error('Plugin device part is too small')
        if (encoded + character.encoded > limit) {
            yield { part: value.slice(start, index), byteLength }
            start = index; encoded = 2; byteLength = 0
        }
        encoded += character.encoded; byteLength += character.bytes; index += character.units
    }
    if (index > start) yield { part: value.slice(start, index), byteLength }
}

type NativeMutation = PluginDeviceMutation | { type: 'staged'; sessionId: string }
function requestSize(owner: string, mutations: readonly NativeMutation[], observations: readonly PluginDeviceObservation[]): number {
    const empty = mutations.map((mutation) => mutation.type === 'set' ? { ...mutation, value: '' } : mutation)
    return encoder.encode(JSON.stringify({ owner, mutations: empty, observations })).length
        + mutations.reduce((size, mutation) => size + (mutation.type === 'set' ? pluginDeviceValueSize(mutation.value) - 2 : 0), 0)
}

async function send<T>(command: string, request: Record<string, unknown>, bytes: number, finish: string): Promise<T> {
    if (isTauriAndroid && bytes > ANDROID_LARGE_COMMIT_SIZE) {
        return await sendAndroidCommit<T>(encoder.encode(JSON.stringify(request)), invoke, undefined, finish)
    }
    return await invoke<T>(command, request)
}

export async function writeNativePluginDeviceValues(owner: string, mutations: readonly PluginDeviceMutation[], observations: readonly PluginDeviceObservation[] = []): Promise<PluginDeviceObservation[]> {
    if (mutations.length === 0) return []
    if (mutations.length > 1024) throw new Error('Plugin device write batch is too large')
    const sizes = mutations.map((mutation) => mutation.type === 'set' ? pluginDeviceValueSize(mutation.value) : 0)
    const planned = mutations.map((mutation, index): NativeMutation => sizes[index] > PLUGIN_DEVICE_PART_BYTES
        ? { type: 'staged', sessionId: '00000000-0000-0000-0000-000000000000' } : mutation)
    if (requestSize(owner, planned, observations) > REQUEST_BYTES) throw new Error('Plugin device write batch is too large')
    const sessions: string[] = []
    try {
        for (let index = 0; index < mutations.length; index++) {
            const mutation = mutations[index]
            if (mutation.type !== 'set' || sizes[index] <= PLUGIN_DEVICE_PART_BYTES) continue
            const { sessionId } = await invoke<{ sessionId: string }>('pds_open_plugin_device_write', {
                owner, space: mutation.space, key: mutation.key, encodedLength: sizes[index],
            })
            sessions.push(sessionId)
            let offset = 0
            for (const { part, byteLength } of pluginDeviceParts(mutation.value)) {
                const request = { owner, sessionId, offset, part }
                const size = pluginDeviceValueSize(part) - 2 + encoder.encode(JSON.stringify({ ...request, part: '' })).length
                const next = await send<number>('pds_append_plugin_device_write', request, size, 'pds_plugin_part_android_finish')
                if (next !== offset + byteLength) throw new Error('Invalid plugin device part acknowledgement')
                offset = next
            }
            planned[index] = { type: 'staged', sessionId }
        }
        return await send<PluginDeviceObservation[]>('pds_write_plugin_device_values', { owner, mutations: planned, observations }, requestSize(owner, planned, observations), 'pds_plugin_write_android_finish') ?? []
    } finally {
        await Promise.all(sessions.map((sessionId) => invoke('pds_abort_plugin_device_write', { owner, sessionId }).catch(() => undefined)))
    }
}

type ReadStart = string | { byteSize: number; version: string } | null
type ReadRange = { type: 'changed' } | { type: 'value'; value: string; nextOffset: number }

export async function readNativePluginDeviceValue(owner: string, space: PluginDeviceSpace, key: string): Promise<string | null> {
    for (let attempt = 0; attempt < 3; attempt++) {
        const opened = await invoke<ReadStart>('pds_read_plugin_device_value', { owner, space, key })
        if (opened == null) return null
        if (typeof opened === 'string') return opened
        const parts: string[] = []
        let offset = 0, changed = false
        while (offset < opened.byteSize) {
            const range = await invoke<ReadRange>('pds_read_plugin_device_range', { owner, space, key, version: opened.version, offset })
            if (range.type === 'changed') { changed = true; break }
            if (range.nextOffset <= offset || range.nextOffset > opened.byteSize) throw new Error('Invalid plugin device read range')
            parts.push(range.value)
            offset = range.nextOffset
        }
        if (!changed) return parts.join('')
    }
    throw new Error('Plugin device value changed during read')
}
