import { describe, expect, it } from 'vitest'
import { FetchBodyPreview, previewFetchRequest } from './boundedFetchPreview'

describe('bounded native fetch preview', () => {
    it('accepts a bodyless GET and preserves small text', () => {
        expect(previewFetchRequest()).toBe('')
        expect(previewFetchRequest('hello')).toBe('hello')
        expect(previewFetchRequest(undefined, new TextEncoder().encode('안녕'))).toBe('안녕')
    })
    it('limits large requests and marks the preview', () => {
        expect(previewFetchRequest('x'.repeat(1024 * 1024))).toBe('x'.repeat(256 * 1024) + '\n[Body preview truncated]')
    })
    it('keeps split UTF-8 while bounding a long SSE stream', () => {
        const preview = new FetchBodyPreview(), bytes = new TextEncoder().encode('😀')
        preview.append(bytes.subarray(0, 2)); preview.append(bytes.subarray(2))
        expect(preview.finish()).toBe('😀')
        const large = new FetchBodyPreview()
        for (let i = 0; i < 100; i++) large.append(new Uint8Array(65536).fill(97))
        expect(large.finish()).toBe('a'.repeat(256 * 1024) + '\n[Body preview truncated]')
    })
})
