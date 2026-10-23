import { describe, expect, it } from 'vitest'
import {
    addGalleryAssets, assetGalleryItems, assetGalleryPage, assetSectionCount, assertAssetSnapshots, deleteGalleryAssets,
    renameGalleryAsset, replaceGalleryAsset, setGalleryCharacterIcon, setGalleryPromptExcluded, selectionAfterAssetDelete,
    AssetNameError, StaleAssetSelectionError, type AssetOwnerData, type AssetOwner,
} from './assetGallery'

const owner: AssetOwner = { kind: 'character', id: 'synthetic-character' }
const entries = (data: AssetOwnerData, section: 'icons' | 'emotions' | 'assets' = 'assets') => [...assetGalleryItems(owner, data, section)]

describe('asset gallery registration operations', () => {
    it('keeps other selected registrations after viewer deletion shifts their positions', () => {
        const data: AssetOwnerData = { additionalAssets: [['first', 'shared', 'png'], ['second', 'shared', 'png'], ['third', 'three', 'png']] }
        const [first, second, third] = entries(data)
        deleteGalleryAssets(owner, data, [first])
        const selected = selectionAfterAssetDelete(owner, data, [second, third], [first])
        expect([...selected.values()].map(entry => [entry.name, entry.position])).toEqual([['second', 0], ['third', 1]])
        assertAssetSnapshots(owner, data, [...selected.values()])
        deleteGalleryAssets(owner, data, [...selected.values()])
        expect(data.additionalAssets).toEqual([])
    })
    it('manages shared content independently by registration and keeps exclusion until its last use', () => {
        const data: AssetOwnerData = { additionalAssets: [['smile', 'assets/shared.png', 'png'], ['happy', 'assets/shared.png', 'png']], prebuiltAssetExclude: ['assets/shared.png'] }
        const [first, second] = entries(data)
        expect(first.id).not.toBe(second.id)
        renameGalleryAsset(owner, data, first, 'smiling')
        expect(data.additionalAssets?.map(tuple => tuple[0])).toEqual(['smiling', 'happy'])
        replaceGalleryAsset(owner, data, entries(data)[0], 'assets/new.webp', 'webp')
        expect(data.additionalAssets).toEqual([['smiling', 'assets/new.webp', 'webp'], ['happy', 'assets/shared.png', 'png']])
        expect(data.prebuiltAssetExclude).toEqual(['assets/shared.png', 'assets/new.webp'])
        deleteGalleryAssets(owner, data, [entries(data)[0]])
        expect(data.prebuiltAssetExclude).toEqual(['assets/shared.png'])
        deleteGalleryAssets(owner, data, entries(data))
        expect(data.prebuiltAssetExclude).toEqual([])
    })

    it('retains exclusions while another section or the portrait still references the key', () => {
        const data: AssetOwnerData = { image: 'shared', emotionImages: [['happy', 'shared']], additionalAssets: [['extra', 'shared', 'png']], prebuiltAssetExclude: ['shared'] }
        deleteGalleryAssets(owner, data, entries(data))
        deleteGalleryAssets(owner, data, entries(data, 'emotions'))
        expect(data.prebuiltAssetExclude).toEqual(['shared'])
        expect(data.image).toBe('shared')
    })

    it('refuses a stale bulk selection atomically, including renamed or reordered registrations', () => {
        const data: AssetOwnerData = { additionalAssets: [['first', 'one', 'png'], ['second', 'two', 'png']] }
        const snapshots = entries(data)
        data.additionalAssets![1][0] = 'renamed elsewhere'
        expect(() => deleteGalleryAssets(owner, data, snapshots)).toThrow(StaleAssetSelectionError)
        expect(data.additionalAssets).toHaveLength(2)
        data.additionalAssets!.reverse()
        expect(() => replaceGalleryAsset(owner, data, snapshots[0], 'new', 'webp')).toThrow(StaleAssetSelectionError)
        expect(data.additionalAssets!.map(tuple => tuple[1])).toEqual(['two', 'one'])
        expect(() => assertAssetSnapshots({ kind: 'character', id: 'other' }, data, snapshots)).toThrow(StaleAssetSelectionError)
    })

    it('rejects empty or case-insensitive duplicate names within a section', () => {
        const data: AssetOwnerData = { additionalAssets: [['Smile', 'one', 'png'], ['happy', 'two', 'png']], emotionImages: [['other', 'three']] }
        expect(() => renameGalleryAsset(owner, data, entries(data)[1], ' smile ')).toThrow(AssetNameError)
        expect(() => renameGalleryAsset(owner, data, entries(data)[1], '  ')).toThrow(AssetNameError)
        renameGalleryAsset(owner, data, entries(data)[1], ' other ')
        expect(data.additionalAssets![1][0]).toBe('other')
    })

    it('adds filename stems and allocates case-insensitive suffixes across the entire batch', () => {
        const data: AssetOwnerData = { additionalAssets: [['Smile', 'one', 'PNG']] }
        expect(entries(data)[0].type).toBe('image')
        addGalleryAssets(owner, data, 'assets', [
            { name: 'smile.png', key: 'two', ext: 'png' },
            { name: 'smile.webp', key: 'three', ext: 'webp' },
        ])
        expect(data.additionalAssets!.map(tuple => tuple[0])).toEqual(['Smile', 'smile (2)', 'smile (3)'])
    })

    it('swaps an alternate icon using upstream portrait archiving and never deletes the portrait', () => {
        const data: AssetOwnerData = { image: 'assets/portrait.png', ccAssets: [{ type: 'icon', name: 'alternate', uri: 'assets/alternate.webp', ext: 'webp' }] }
        setGalleryCharacterIcon(owner, data, entries(data, 'icons')[1])
        expect(data.image).toBe('assets/alternate.webp')
        expect(data.ccAssets).toEqual([{ type: 'icon', name: 'iconx', uri: 'assets/portrait.png', ext: 'png' }])
        deleteGalleryAssets(owner, data, entries(data, 'icons'))
        expect(data.image).toBe('assets/alternate.webp')
        expect(data.ccAssets).toEqual([])
    })

    it('edits the prompt exclusion once per shared key and only while prompt insertion is enabled', () => {
        const data: AssetOwnerData = { additionalAssets: [['smile', 'shared', 'png'], ['happy', 'shared', 'png']] }
        setGalleryPromptExcluded(owner, data, entries(data)[0], true)
        expect(data.prebuiltAssetExclude).toBeUndefined()
        data.prebuiltAssetCommand = true
        setGalleryPromptExcluded(owner, data, entries(data)[0], true)
        setGalleryPromptExcluded(owner, data, entries(data)[1], true)
        expect(data.prebuiltAssetExclude).toEqual(['shared'])
        setGalleryPromptExcluded(owner, data, entries(data)[1], false)
        expect(data.prebuiltAssetExclude).toEqual([])
    })

    it('removes a large registration selection without changing the active greeting', () => {
        const data = { additionalAssets: Array.from({ length: 5000 }, (_, index): [string, string, string] => [`asset ${index}`, `key ${index}`, 'png']), chats: [{ fmIndex: 3 }] }
        deleteGalleryAssets(owner, data, entries(data))
        expect(data.additionalAssets).toEqual([])
        expect(data.chats[0].fmIndex).toBe(3)
    })

    it('uses the module assets field without affecting character fields', () => {
        const moduleOwner: AssetOwner = { kind: 'module', id: 'synthetic-module' }
        const data: AssetOwnerData = { assets: [['module', 'old', 'png']], additionalAssets: [['character', 'other', 'png']] }
        const [entry] = [...assetGalleryItems(moduleOwner, data, 'assets')]
        replaceGalleryAsset(moduleOwner, data, entry, 'new', 'webp')
        expect(data.assets).toEqual([['module', 'new', 'webp']])
        expect(data.additionalAssets).toEqual([['character', 'other', 'png']])
        expect([...assetGalleryItems(moduleOwner, data, 'icons')]).toEqual([])
    })
    it('reads only the requested unfiltered positions, including a far window', () => {
        const tuples: [string, string, string][] = Array.from({ length: 5000 }, (_, index) => [`asset ${index}`, `key ${index}`, 'png'])
        Object.defineProperty(tuples, 0, { get() { throw new Error('Off-window tuple read') } })
        const data: AssetOwnerData = { additionalAssets: tuples }
        expect(assetSectionCount(owner, data, 'assets')).toBe(5000)
        const page = assetGalleryPage(owner, data, 'assets', '', 'all', 4998, 8)
        expect([...page].map(([index, entry]) => [index, entry.position, entry.name])).toEqual([[4998, 4998, 'asset 4998'], [4999, 4999, 'asset 4999']])
        expect(assetGalleryPage(owner, data, 'assets', '', 'all', -1, 1).size).toBe(0)
    })

    it('keeps filtered order and stops after the last requested match', () => {
        const tuples: [string, string, string][] = [['match one', '1', 'png'], ['skip', '2', 'png'], ['MATCH audio', '3', 'wav'], ['MATCH two', '4', 'png'], ['match three', '5', 'png']]
        Object.defineProperty(tuples, 4, { get() { throw new Error('Tuple after page read') } })
        const page = assetGalleryPage(owner, { additionalAssets: tuples }, 'assets', 'match', 'image', 1, 1)
        expect([...page].map(([index, entry]) => [index, entry.position, entry.name])).toEqual([[1, 3, 'MATCH two']])
    })

    it('maps portrait-first display indices to registration positions for character and module pages', () => {
        const data: AssetOwnerData = { image: 'portrait.png', ccAssets: [{ type: 'icon', uri: 'alternate.png', name: 'alternate', ext: 'png' }], assets: [['module', 'module.png', 'png']] }
        expect([...assetGalleryPage(owner, data, 'icons', '', 'all', 0, 3).values()].map(entry => [entry.name, entry.position])).toEqual([['', -1], ['alternate', 0]])
        delete data.image
        expect(assetGalleryPage(owner, data, 'icons', '', 'all', 0, 1).get(0)?.position).toBe(0)
        const moduleOwner: AssetOwner = { kind: 'module', id: 'module' }
        expect(assetSectionCount(moduleOwner, data, 'icons')).toBe(0)
        expect(assetGalleryPage(moduleOwner, data, 'icons', '', 'all', 0, 3).size).toBe(0)
        expect(assetGalleryPage(moduleOwner, data, 'assets', '', 'all', 0, 3).get(0)?.name).toBe('module')
    })

})
