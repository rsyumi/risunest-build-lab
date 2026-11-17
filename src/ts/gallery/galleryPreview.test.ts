import { describe, expect, it } from 'vitest'
import { createTauriCasObjectUrl } from '../storage/platformBlobStore'
import { galleryPreviewSource } from './galleryPreview'
import type { GalleryItem } from './types'

const item: GalleryItem = { id:'image', key:'image', kind:'inlay', name:'photo.png', ext:'png',
    mime:'image/png', size:1234, type:'image', width:2048, height:1024 }
const endpoint = `http://127.0.0.1:40123/${'a'.repeat(32)}/`
function source(contentHash = 'b'.repeat(64), mime = 'image/png') {
    return { url:createTauriCasObjectUrl({contentHash,mime,size:1234},endpoint), width:2048,height:1024,size:1234 }
}

describe('native gallery image previews', () => {
    it('requests a hash-addressed derivative while retaining original metadata and URL identity', () => {
        const original=source()
        for (const kind of ['inlay','asset'] as const) {
            const preview=galleryPreviewSource({...item,kind},original,true)!
            const url=new URL(preview.url)
            expect(url.searchParams.get('thumbnail')).toBe('gallery-v1')
            expect(url.pathname).toBe(new URL(original.url).pathname)
            expect(url.searchParams.get('mime')).toBe('image/png')
            expect(url.searchParams.get('size')).toBe('1234')
            expect(preview).toMatchObject({thumbnail:true,width:2048,height:1024,size:1234})
            expect(original.url).not.toContain('thumbnail')
        }
        expect(galleryPreviewSource(item,source('c'.repeat(64)),true)!.url)
            .not.toBe(galleryPreviewSource(item,original,true)!.url)
    })
    it('keeps viewer, web, non-image, unsupported format and external sources unchanged', () => {
        const original=source()
        expect(galleryPreviewSource(item,original,false)).toBe(original)
        expect(galleryPreviewSource({...item,type:'video'},original,true)).toBe(original)
        const svg=source('b'.repeat(64),'image/svg+xml')
        expect(galleryPreviewSource({...item,ext:'svg',mime:'image/svg+xml'},svg,true)).toBe(svg)
        for (const url of ['https://example.invalid/photo.png','blob:synthetic','data:image/png;base64,AA==']) {
            const external={...original,url}
            expect(galleryPreviewSource(item,external,true)).toBe(external)
        }
        expect(galleryPreviewSource(item,null,true)).toBeNull()
    })
    it('does not persist reduced dimensions under the original content hash', async () => {
        let geometry={width:2048,height:1024}
        const original={...source(),recordDimensions:async (width:number,height:number) => { geometry={width,height} }}
        const preview=galleryPreviewSource(item,original,true)!
        await preview.recordDimensions?.(512,256)
        expect(geometry).toEqual({width:2048,height:1024})
        expect(preview.recordDimensions).toBeUndefined()
        await galleryPreviewSource(item,original,false)!.recordDimensions?.(2048,1024)
        expect(geometry).toEqual({width:2048,height:1024})
    })
})
