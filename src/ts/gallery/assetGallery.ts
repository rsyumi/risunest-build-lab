import { RevisionConflictError, type PersistentRevisionReader } from '../storage/persistentDataStore'
import type { PreparedUnitIntent } from '../storage/saveCoordinator'
import type { GalleryItem } from './types'
import { archiveCurrentCharacterImage } from '../storage/characterImageFileRoute'

export type AssetSection = 'icons' | 'emotions' | 'assets'
export type AssetOwner = { kind: 'character' | 'module'; id: string }
export interface AssetOwnerData {
    image?: string
    ccAssets?: { type: string; uri: string; name: string; ext: string }[]
    emotionImages?: [string, string][]
    additionalAssets?: [string, string, string][]
    assets?: [string, string, string][]
    prebuiltAssetCommand?: boolean
    prebuiltAssetExclude?: string[]
}
export interface AssetGalleryItem extends GalleryItem {
    owner: AssetOwner
    section: AssetSection
    position: number
}
export class StaleAssetSelectionError extends Error {
    constructor() { super('Asset registrations changed'); this.name = 'StaleAssetSelectionError' }
}
export class AssetNameError extends Error {
    constructor(readonly reason: 'empty' | 'duplicate') { super(reason) }
}
const normalizedName = (name: string) => name.trim().toLocaleLowerCase()
export const assetExtension = (name: string) => name.includes('.') ? name.split('.').pop()!.toLowerCase() : ''
export function assetMediaType(ext: string): GalleryItem['type'] {
    ext = ext.toLocaleLowerCase('en-US')
    if (['png', 'webp', 'gif', 'jpg', 'jpeg', 'avif', 'svg', 'bmp'].includes(ext)) return 'image'
    if (['mp4', 'webm', 'mov', 'm4v'].includes(ext)) return 'video'
    if (['mp3', 'wav', 'ogg', 'flac', 'm4a', 'aac'].includes(ext)) return 'audio'
    return 'other'
}
function item(owner: AssetOwner, section: AssetSection, position: number, name: string, key: string, ext: string, portrait = false): AssetGalleryItem {
    return { id: JSON.stringify([owner.kind, owner.id, section, position]), kind: 'asset', owner, section, position, name, key, ext, type: assetMediaType(ext), mime: '', size: 0, portrait }
}
export function* assetGalleryItems(owner: AssetOwner, data: AssetOwnerData, section: AssetSection): Generator<AssetGalleryItem> {
    if (owner.kind === 'module') {
        if (section === 'assets') for (const [position, tuple] of (data.assets ?? []).entries()) yield item(owner, section, position, tuple[0], tuple[1], tuple[2] || assetExtension(tuple[1]))
    } else if (section === 'icons') {
        if (data.image) yield item(owner, section, -1, '', data.image, assetExtension(data.image), true)
        for (const [position, icon] of (data.ccAssets ?? []).entries()) yield item(owner, section, position, icon.name, icon.uri, icon.ext || assetExtension(icon.uri))
    } else if (section === 'emotions') {
        for (const [position, tuple] of (data.emotionImages ?? []).entries()) yield item(owner, section, position, tuple[0], tuple[1], assetExtension(tuple[1]))
    } else {
        for (const [position, tuple] of (data.additionalAssets ?? []).entries()) yield item(owner, section, position, tuple[0], tuple[1], tuple[2] || assetExtension(tuple[1]))
    }
}
export function assetAt(owner: AssetOwner, data: AssetOwnerData, section: AssetSection, position: number): AssetGalleryItem | undefined {
    if (owner.kind === 'module' && section !== 'assets') return
    if (section === 'icons') {
        if (position === -1) return data.image ? item(owner, section, -1, '', data.image, assetExtension(data.image), true) : undefined
        const icon = data.ccAssets?.[position]
        return icon ? item(owner, section, position, icon.name, icon.uri, icon.ext || assetExtension(icon.uri)) : undefined
    }
    const tuple = section === 'emotions' ? data.emotionImages?.[position] : (owner.kind === 'module' ? data.assets : data.additionalAssets)?.[position]
    return tuple ? item(owner, section, position, tuple[0], tuple[1], tuple[2] || assetExtension(tuple[1])) : undefined
}
export function assetSectionCount(owner: AssetOwner, data: AssetOwnerData, section: AssetSection): number {
    if (owner.kind === 'module') return section === 'assets' ? data.assets?.length ?? 0 : 0
    if (section === 'icons') return (data.image ? 1 : 0) + (data.ccAssets?.length ?? 0)
    return (section === 'emotions' ? data.emotionImages : data.additionalAssets)?.length ?? 0
}
export function assetGalleryPage(owner: AssetOwner, data: AssetOwnerData, section: AssetSection, search: string, filter: GalleryItem['type'] | 'all', offset: number, limit: number): Map<number, AssetGalleryItem> {
    const items = new Map<number, AssetGalleryItem>()
    if (offset < 0 || limit <= 0) return items
    if (!search && filter === 'all') {
        const portraitOffset = owner.kind === 'character' && section === 'icons' && data.image ? 1 : 0
        const end = Math.min(assetSectionCount(owner, data, section), offset + limit)
        for (let index = offset; index < end; index++) {
            const entry = assetAt(owner, data, section, index - portraitOffset)
            if (entry) items.set(index, entry)
        }
        return items
    }
    let index = 0
    for (const entry of assetGalleryItems(owner, data, section)) {
        if ((filter !== 'all' && entry.type !== filter) || (search && !entry.name.toLocaleLowerCase().includes(search))) continue
        if (index >= offset) items.set(index, entry)
        if (++index >= offset + limit) break
    }
    return items
}
export function assertAssetSnapshots(owner: AssetOwner, data: AssetOwnerData, snapshots: readonly AssetGalleryItem[]): void {
    for (const snapshot of snapshots) {
        const current = assetAt(owner, data, snapshot.section, snapshot.position)
        if (snapshot.owner.kind !== owner.kind || snapshot.owner.id !== owner.id || !current || current.name !== snapshot.name || current.key !== snapshot.key || current.ext !== snapshot.ext) throw new StaleAssetSelectionError()
    }
}
export function validateAssetName(owner: AssetOwner, data: AssetOwnerData, section: AssetSection, name: string, exceptPosition?: number): void {
    if (!normalizedName(name)) throw new AssetNameError('empty')
    for (const entry of assetGalleryItems(owner, data, section)) {
        if (!entry.portrait && entry.position !== exceptPosition && normalizedName(entry.name) === normalizedName(name)) throw new AssetNameError('duplicate')
    }
}
export function uniqueAssetName(owner: AssetOwner, data: AssetOwnerData, section: AssetSection, filename: string): string {
    const base = filename.replace(/\.[^.]+$/, '').trim() || filename
    const names = new Set(Array.from(assetGalleryItems(owner, data, section), entry => normalizedName(entry.name)))
    let name = base
    for (let suffix = 2; names.has(normalizedName(name)); suffix++) name = `${base} (${suffix})`
    return name
}
function removeUnusedExclusions(owner: AssetOwner, data: AssetOwnerData, candidates: ReadonlySet<string>) {
    if (owner.kind !== 'character' || !data.prebuiltAssetExclude) return
    const used = new Set<string>()
    for (const section of ['icons', 'emotions', 'assets'] as const) for (const entry of assetGalleryItems(owner, data, section)) used.add(entry.key)
    data.prebuiltAssetExclude = data.prebuiltAssetExclude.filter(key => !candidates.has(key) || used.has(key))
}
export function deleteGalleryAssets(owner: AssetOwner, data: AssetOwnerData, snapshots: readonly AssetGalleryItem[]): void {
    assertAssetSnapshots(owner, data, snapshots)
    const removable = snapshots.filter(entry => !entry.portrait)
    for (const section of ['icons', 'emotions', 'assets'] as const) {
        const positions = new Set(removable.filter(entry => entry.section === section).map(entry => entry.position))
        if (!positions.size) continue
        if (section === 'icons') data.ccAssets = data.ccAssets?.filter((_, index) => !positions.has(index))
        else if (section === 'emotions') data.emotionImages = data.emotionImages?.filter((_, index) => !positions.has(index))
        else if (owner.kind === 'module') data.assets = data.assets?.filter((_, index) => !positions.has(index))
        else data.additionalAssets = data.additionalAssets?.filter((_, index) => !positions.has(index))
    }
    removeUnusedExclusions(owner, data, new Set(removable.map(entry => entry.key)))
}
export function selectionAfterAssetDelete(owner: AssetOwner, data: AssetOwnerData, selected: readonly AssetGalleryItem[], removed: readonly AssetGalleryItem[]): Map<string, AssetGalleryItem> {
    const result = new Map<string, AssetGalleryItem>()
    const deleted = new Set(removed.map(entry => entry.id))
    const positions = new Map<AssetSection, number[]>()
    for (const entry of removed) {
        const values = positions.get(entry.section) ?? []
        values.push(entry.position); positions.set(entry.section, values)
    }
    for (const values of positions.values()) values.sort((a, b) => a - b)
    for (const entry of selected) {
        if (deleted.has(entry.id)) continue
        const values = positions.get(entry.section) ?? []
        let low = 0, high = values.length
        while (low < high) {
            const middle = (low + high) >>> 1
            if (values[middle] < entry.position) low = middle + 1
            else high = middle
        }
        const position = entry.position - low
        const current = assetAt(owner, data, entry.section, position)
        if (current && current.key === entry.key && current.name === entry.name && current.ext === entry.ext) result.set(current.id, current)
    }
    return result
}
export function renameGalleryAsset(owner: AssetOwner, data: AssetOwnerData, snapshot: AssetGalleryItem, name: string): void {
    assertAssetSnapshots(owner, data, [snapshot])
    if (snapshot.portrait) return
    validateAssetName(owner, data, snapshot.section, name, snapshot.position)
    name = name.trim()
    if (snapshot.section === 'icons') data.ccAssets![snapshot.position].name = name
    else if (snapshot.section === 'emotions') data.emotionImages![snapshot.position][0] = name
    else (owner.kind === 'module' ? data.assets! : data.additionalAssets!)[snapshot.position][0] = name
}
export function replaceGalleryAsset(owner: AssetOwner, data: AssetOwnerData, snapshot: AssetGalleryItem, key: string, ext: string): void {
    assertAssetSnapshots(owner, data, [snapshot])
    if (snapshot.portrait) throw new Error('Portrait replacement uses selectCharImg')
    if (snapshot.section === 'icons') Object.assign(data.ccAssets![snapshot.position], { uri: key, ext })
    else if (snapshot.section === 'emotions') data.emotionImages![snapshot.position][1] = key
    else {
        const tuple = (owner.kind === 'module' ? data.assets! : data.additionalAssets!)[snapshot.position]
        tuple[1] = key; tuple[2] = ext
        if (owner.kind === 'character' && data.prebuiltAssetExclude?.includes(snapshot.key) && !data.prebuiltAssetExclude.includes(key)) data.prebuiltAssetExclude.push(key)
    }
    removeUnusedExclusions(owner, data, new Set([snapshot.key]))
}
export function addGalleryAssets(owner: AssetOwner, data: AssetOwnerData, section: AssetSection, files: readonly { name: string; key: string; ext: string }[]): void {
    for (const file of files) {
        const name = uniqueAssetName(owner, data, section, file.name)
        if (section === 'icons') (data.ccAssets ??= []).push({ type: 'icon', name, uri: file.key, ext: file.ext })
        else if (section === 'emotions') (data.emotionImages ??= []).push([name, file.key])
        else if (owner.kind === 'module') (data.assets ??= []).push([name, file.key, file.ext])
        else (data.additionalAssets ??= []).push([name, file.key, file.ext])
    }
}
export function setGalleryCharacterIcon(owner: AssetOwner, data: AssetOwnerData, snapshot: AssetGalleryItem): void {
    assertAssetSnapshots(owner, data, [snapshot])
    if (owner.kind !== 'character' || snapshot.section !== 'icons' || snapshot.portrait) return
    data.ccAssets!.splice(snapshot.position, 1)
    archiveCurrentCharacterImage(data)
    data.image = snapshot.key
}
export function setGalleryPromptExcluded(owner: AssetOwner, data: AssetOwnerData, snapshot: AssetGalleryItem, excluded: boolean): void {
    assertAssetSnapshots(owner, data, [snapshot])
    if (owner.kind !== 'character' || snapshot.section !== 'assets' || !data.prebuiltAssetCommand) return
    const keys = new Set(data.prebuiltAssetExclude ?? [])
    if (excluded) keys.add(snapshot.key)
    else keys.delete(snapshot.key)
    data.prebuiltAssetExclude = [...keys]
}

export async function readPinnedAssetOwner(reader: PersistentRevisionReader, owner: AssetOwner): Promise<AssetOwnerData> {
    if (owner.kind === 'character') {
        const record = await reader.readCharacter(owner.id)
        if (!record) throw new StaleAssetSelectionError()
        if (record.revision !== reader.revision) throw new RevisionConflictError(reader.revision, record.revision)
        return record.value
    }
    const record = await reader.readRoot()
    if (record.revision !== reader.revision) throw new RevisionConflictError(reader.revision, record.revision)
    const module = record.value.modules?.find(value => value.id === owner.id)
    if (!module) throw new StaleAssetSelectionError()
    return module
}

export async function prepareGalleryModuleMutation(
    reader: PersistentRevisionReader,
    moduleId: string,
    mutate: (data: AssetOwnerData) => void,
): Promise<PreparedUnitIntent> {
    const module = structuredClone(await readPinnedAssetOwner(reader, { kind: 'module', id: moduleId }))
    mutate(module)
    return { unitMutations: [{ key: JSON.stringify(['record', 'modules', moduleId]), type: 'set', value: module }], conversations: [] }
}
