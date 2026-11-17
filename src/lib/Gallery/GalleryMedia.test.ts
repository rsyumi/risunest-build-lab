import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { mount, tick, unmount } from 'svelte'
import { get } from 'svelte/store'
import type { GalleryItem } from 'src/ts/gallery/types'
import { resolveGalleryMedia, type GalleryMediaSource } from 'src/ts/gallery/galleryMedia'

const storage = vi.hoisted(() => {
    vi.useFakeTimers({toFake:['queueMicrotask']})
    vi.stubGlobal('__TAURI_INTERNALS__', {metadata:{currentWindow:{label:'gallery-test'},currentWebview:{label:'gallery-test'}}})
    return { hash:'a'.repeat(64), endpoint:'a'.repeat(32), pending:null as Promise<void> | null,
        geometry:[] as {contentHash:string;width:number;height:number}[], rejectImageReads:false, releasePending:undefined as (()=>void) | undefined }
})
vi.mock('@tauri-apps/api/core', async importOriginal => ({
    ...await importOriginal<typeof import('@tauri-apps/api/core')>(),
    async invoke<T>(command:string, args?:Parameters<typeof import('@tauri-apps/api/core').invoke>[1]):Promise<T> {
        let result:unknown
        if (storage.rejectImageReads && ['pds_read_asset_alias','asset_cas_stat_object','pds_read_image_geometry'].includes(command)) throw new Error('Preview repeated a metadata IPC')
        switch(command) {
            case 'pds_read_asset_alias': {
                const value = {kind:'inlay',key:'stable',inlayType:'image',objectHash:storage.hash,
                    name:'first.png',ext:'png',mime:'image/png',size:5,width:1024,height:768}
                await storage.pending
                result = {revision:1,value}; break
            }
            case 'asset_cas_stat_object': result = 5; break
            case 'native_media_base_url': case 'native_media_ensure':
                result = `http://127.0.0.1:40123/${storage.endpoint}/`; break
            case 'pds_read_image_geometry': result = []; break
            case 'pds_write_image_geometry':
                storage.geometry.push(...(args as {values:typeof storage.geometry}).values); break
            default: throw new Error(`Unexpected native command: ${command}`)
        }
        return result as T
    },
} satisfies typeof import('@tauri-apps/api/core')))
import { configureActiveBlobStore, getNativeMediaEndpoint, createTauriCasObjectUrl } from 'src/ts/storage/platformBlobStore'
import { createNativeAssetBlobStore } from 'src/ts/storage/assetRepositoryRuntime'
import { SqlitePersistentDataStore } from 'src/ts/storage/sqlitePersistentDataStore'
import { createNativeImageGeometryStore } from 'src/ts/storage/imageGeometry'
import { createNativeImmutablePayloadCas, createNativeAssetObjectUrlResolver,
    createNativeNewInlayImageEncoder, createNativeDurableAssetWriteSessionFactory } from 'src/ts/storage/nativeAssetRepository'
import { createInRealmStorageLockManager, createStorageMutationGate } from 'src/ts/storage/storageMutationGate'
import Harness from './GalleryMediaHarness.test.svelte'
import GalleryViewer from './GalleryViewer.svelte'
import GalleryGrid from './GalleryGrid.svelte'
import { alertStore } from 'src/ts/stores.svelte'

// Flush startup effects after Vitest's asynchronous module graph has initialized.
vi.runAllTicks()
vi.useRealTimers()

const initial:GalleryItem = { id:'stable', key:'stable', kind:'inlay', objectHash:'a'.repeat(64),
    type:'image', name:'first.png', ext:'png', mime:'image/png', size:5, width:1024, height:768 }
let component:ReturnType<typeof mount> | undefined
let target:HTMLDivElement
function render(onmetadata?:(source:GalleryMediaSource)=>void) {
    target = document.createElement('div'); document.body.appendChild(target)
    const instance = mount(Harness, {target,props:{initial,onmetadata}})
    component = instance
    return instance
}
async function image() { await vi.waitFor(() => expect(target.querySelector('img')).not.toBeNull()); return target.querySelector('img')! }
function contentUrl() { return createTauriCasObjectUrl({contentHash:storage.hash,mime:'image/png',size:5},`http://127.0.0.1:40123/${storage.endpoint}/`) }
async function settled() { await tick(); await Promise.resolve(); await tick() }

beforeEach(async () => {
    storage.hash = initial.objectHash!; storage.endpoint = 'a'.repeat(32); storage.pending = null; storage.geometry = []; storage.rejectImageReads = false
    vi.stubGlobal('IntersectionObserver', undefined)
    configureActiveBlobStore(createStorageMutationGate({locks:createInRealmStorageLockManager()}), createNativeAssetBlobStore({
        store:new SqlitePersistentDataStore(), cas:createNativeImmutablePayloadCas(), objectUrls:createNativeAssetObjectUrlResolver(),
        imageGeometry:createNativeImageGeometryStore(), newInlayImages:createNativeNewInlayImageEncoder(),
        writeSessions:createNativeDurableAssetWriteSessionFactory(),
    }), {alreadyGuarded:true})
    await getNativeMediaEndpoint.ensure()
})
afterEach(async () => {
    storage.releasePending?.(); storage.releasePending = undefined; storage.pending = null
    await settled()
    if(component) await unmount(component)
    component = undefined
    await settled()
    document.body.replaceChildren(); vi.unstubAllGlobals()
    alertStore.set({type:'none',msg:''})
})

describe('gallery navigation and focus', () => {
    it('navigates only from its own dialog and leaves media, pending alerts and overlays alone', async () => {
        target = document.createElement('div'); document.body.appendChild(target)
        const navigated:number[] = []
        alertStore.set({type:'none',msg:''})
        component = mount(GalleryViewer, { target, props: { item:{...initial,type:'other'}, index:1, total:3,
            onclose:()=>{}, onnavigate:index=>navigated.push(index) } })
        await settled()
        const viewer = target.querySelector<HTMLElement>('[role="dialog"]')!
        const key = (node:HTMLElement) => node.dispatchEvent(new KeyboardEvent('keydown',{key:'ArrowRight',bubbles:true,cancelable:true}))
        key(viewer.querySelector('button')!)
        expect(navigated).toEqual([2])
        for (const tag of ['video','audio','input']) {
            const content = document.createElement(tag); viewer.appendChild(content); key(content); content.remove()
        }
        const overlay = document.createElement('div'); overlay.setAttribute('role','dialog'); document.body.appendChild(overlay)
        const button = document.createElement('button'); overlay.appendChild(button); key(button)
        alertStore.set({type:'normal',msg:'first'}); alertStore.set({type:'normal',msg:'second'})
        key(viewer)
        alertStore.set({type:'none',msg:''})
        expect(get(alertStore).dialogPending).toBe(true)
        key(viewer)
        await vi.waitFor(() => expect(get(alertStore).type).toBe('normal'))
        alertStore.set({type:'none',msg:''})
        alertStore.set({type:'progress',msg:''}); key(viewer)
        expect(navigated).toEqual([2])
    })
    it('scrolls a focused grid row below the sticky toolbar', async () => {
        target = document.createElement('div')
        target.style.overflowY = 'auto'
        document.body.appendChild(target)
        const toolbar = document.createElement('div'); toolbar.dataset.galleryToolbar = ''; toolbar.style.position = 'sticky'; toolbar.style.top = '0px'
        toolbar.getBoundingClientRect = () => ({top:0,bottom:60,height:60} as DOMRect)
        target.appendChild(toolbar)
        const content = document.createElement('div'); target.appendChild(content)
        const app = mount(GalleryGrid, {target:content,props:{total:100,items:new Map<number, GalleryItem>(),selected:new Set<string>(),onwindow:()=>{},onopen:()=>{},ontoggle:()=>{}}})
        component = app
        await settled()
        const root = content.querySelector<HTMLElement>('[data-gallery-grid]')!
        root.getBoundingClientRect = () => ({top:60-target.scrollTop,bottom:2000,width:600,height:2000} as DOMRect)
        await app.scrollToIndex(0)
        expect(target.scrollTop).toBe(12)
    })
})

describe('gallery media content lifetime', () => {
    it('renders authoritative native previews without repeating alias, size or geometry reads', async () => {
        storage.rejectImageReads = true
        const metadata:GalleryMediaSource[] = [], app = render(value => metadata.push(value))
        app.setPreview(true)
        const preview = await image(), url = new URL(preview.src)
        expect(url.searchParams.get('thumbnail')).toBe('gallery-v1')
        expect(url.searchParams.get('size')).toBe('5')
        expect(url.searchParams.get('mime')).toBe('image/png')
        expect(metadata.at(-1)).toMatchObject({width:1024,height:768,size:5,thumbnail:true})
        expect(metadata.at(-1)?.recordDimensions).toBeUndefined()
        Object.defineProperties(preview,{naturalWidth:{value:512},naturalHeight:{value:384}})
        preview.dispatchEvent(new Event('load'))
        await settled()
        expect(storage.geometry).toEqual([])
    })
    it('keeps original resolution and authoritative lookup for viewers, unsupported formats and incomplete descriptors', async () => {
        storage.hash = 'b'.repeat(64)
        const expected = contentUrl()
        const original = await resolveGalleryMedia(initial)
        expect(original).toMatchObject({url:expected,width:1024,height:768,size:5})
        expect(original?.thumbnail).toBeUndefined()
        expect(original?.recordDimensions).toBeTypeOf('function')
        for (const overrides of [{objectHash:undefined},{objectHash:'invalid'},{size:-1},{size:1.5},{size:NaN},{mime:'image/\npng'},{mime:'image/svg+xml',ext:'svg'}]) {
            const result = await resolveGalleryMedia({...initial,...overrides},true)
            expect(new URL(result!.url).pathname).toBe(new URL(expected).pathname)
        }
    })
    it('retains the loaded node across new page records while updating labels, dimensions and the current callback', async () => {
        const oldMetadata:GalleryMediaSource[] = [], currentMetadata:GalleryMediaSource[] = []
        const app = render(value => oldMetadata.push(value)), before = await image(), url = before.getAttribute('src')
        app.setCallback(value => currentMetadata.push(value))
        app.update({...initial,name:'renamed.png',width:2048,height:1536,size:9,metadata:{createdAt:123}})
        await settled()
        expect(await image()).toBe(before)
        expect(before.getAttribute('src')).toBe(url)
        expect(before.alt).toBe('renamed.png')
        expect([before.width,before.height]).toEqual([2048,1536])
        expect(currentMetadata.at(-1)).toMatchObject({width:2048,height:1536,size:9})
        const previous = oldMetadata.length
        Object.defineProperties(before,{naturalWidth:{value:2048},naturalHeight:{value:1536}})
        before.dispatchEvent(new Event('load'))
        expect(oldMetadata).toHaveLength(previous)
        await vi.waitFor(() => expect(storage.geometry).toEqual([{contentHash:initial.objectHash,width:2048,height:1536}]))
        expect(currentMetadata.at(-1)).toMatchObject({width:2048,height:1536})
    })
    it('replaces the same alias when its content hash changes and clears the detached source', async () => {
        const app = render(), before = await image()
        storage.hash = 'b'.repeat(64)
        app.update({...initial,objectHash:storage.hash})
        await vi.waitFor(() => expect(target.querySelector('img')?.src).toBe(contentUrl()))
        expect(target.querySelector('img')).not.toBe(before)
        expect(before.getAttribute('src')).toBeNull()
        expect(before.isConnected).toBe(false)
    })
    it('refreshes endpoint and preview URLs independently of the alias content', async () => {
        const app = render(); await image()
        storage.endpoint = 'c'.repeat(32); await getNativeMediaEndpoint.ensure()
        await vi.waitFor(() => expect(target.querySelector('img')?.src).toContain(storage.endpoint))
        app.setPreview(true)
        await vi.waitFor(() => expect(target.querySelector('img')?.src).toContain('thumbnail=gallery-v1'))
        const preview = await image()
        Object.defineProperties(preview,{naturalWidth:{value:512},naturalHeight:{value:384}})
        preview.dispatchEvent(new Event('load'))
        expect(storage.geometry).toEqual([])
        app.setPreview(false)
        await vi.waitFor(() => expect(target.querySelector('img')?.src).not.toContain('thumbnail='))
    })
    it('uses fallback revisions for records without a content hash and ignores late replaced content', async () => {
        const current:GalleryMediaSource[] = [], app = render(value => current.push(value)); await image()
        let finish!:() => void
        storage.pending = new Promise(resolve => { finish = resolve; storage.releasePending = resolve })
        storage.hash = 'd'.repeat(64)
        app.update({...initial,objectHash:undefined,contentRevision:2})
        await tick()
        storage.pending = null; storage.hash = 'e'.repeat(64)
        app.update({...initial,objectHash:undefined,contentRevision:3})
        await vi.waitFor(() => expect(target.querySelector('img')?.src).toBe(contentUrl()))
        const before = await image()
        finish(); await settled()
        expect(target.querySelector('img')).toBe(before)
        expect(current.at(-1)?.url).toBe(contentUrl())
        await unmount(app); component = undefined
        expect(before.getAttribute('src')).toBeNull()
    })
})
