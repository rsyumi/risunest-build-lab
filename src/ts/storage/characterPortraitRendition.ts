import { renderPortraitPng } from '../media/portraitRendition'
import { invoke } from '@tauri-apps/api/core'
import { language } from 'src/lang'
import { imageFormatFromBytes } from '../media/imageFormats'
import { getPersistentDataStore } from './persistentDataStoreFactory'
import { withPersistentRevisionLease } from './persistentRecordIterator'
import { createNativeImmutablePayloadCas, createNativeRemoteAssetReader } from './nativeAssetRepository'
import type { CharacterDetail } from './persistentDataStore'
import { recordNativeLogError } from '../nativeLog'

export interface PortraitRenditionDescriptor {
    sourceHash: string
    path: string
}

export interface PreparedPortraitRendition {
    descriptor: PortraitRenditionDescriptor
    release(): Promise<void>
}

const MAX_BYTES = 50 * 1024 * 1024
const PIECE_BYTES = 1024 * 1024

export async function prepareCharacterPortraitRendition(
    character: CharacterDetail,
    revision: number,
    format: 'png-card' | 'appended-charx-jpeg',
    signal?: AbortSignal,
): Promise<PreparedPortraitRendition | undefined> {
    if (!character.image) return undefined
    signal?.throwIfAborted()
    const source = await withPersistentRevisionLease(await getPersistentDataStore().acquireRevision(revision), async reader => {
        const alias = (await reader.readAssetAlias({ kind: 'asset', key: character.image }))?.value
        // An unreadable portrait gets no rendition; the native export writes the fallback image.
        if (!alias?.objectHash) return undefined
        const cas = createNativeImmutablePayloadCas()
        const range = { start: 0, endExclusive: Math.min(alias.size, 64 * 1024) }
        const header = await cas.readObjectRange(alias.objectHash, range, alias.size)
            ?? await createNativeRemoteAssetReader().readObject(alias.objectHash, range)
        if (!header) return undefined
        const detected = imageFormatFromBytes(header)
        if (detected && detected.ext !== 'avif') return undefined
        if (alias.size > MAX_BYTES) throw new Error(language.risuNest.imageFormats.portraitExportFailed)
        const bytes = alias.size <= header.length ? header : await cas.readObject(alias.objectHash, alias.size)
        if (!bytes) throw new Error(language.risuNest.imageFormats.portraitExportFailed)
        const hash = Array.from(new Uint8Array(await crypto.subtle.digest('SHA-256', new Uint8Array(bytes))), byte => byte.toString(16).padStart(2, '0')).join('')
        if (hash !== alias.objectHash) throw new Error(language.risuNest.imageFormats.portraitExportFailed)
        return { bytes, hash }
    })
    if (!source) return undefined
    const bytes = await renderPortraitPng(source.bytes, format, signal)
    signal?.throwIfAborted()
    const path = await invoke<string>('native_download_handoff_create')
    const release = async () => {
        await invoke('native_download_handoff_cleanup', { path }).catch(error => {
            void recordNativeLogError(`Portrait rendition cleanup failed: ${String(error)}`).catch(() => {})
        })
    }
    try {
        for (let offset = 0; offset < bytes.length; offset += PIECE_BYTES) {
            signal?.throwIfAborted()
            const piece = bytes.subarray(offset, offset + PIECE_BYTES)
            const written = await invoke<number>('native_download_handoff_append', {
                path, offset, chunk: Buffer.from(piece).toString('base64'),
            })
            if (written !== offset + piece.length) throw new Error(language.risuNest.imageFormats.portraitExportFailed)
        }
        signal?.throwIfAborted()
        return { descriptor: { sourceHash: source.hash, path }, release }
    } catch (error) { await release(); throw error }
}
