import { renderPortraitPng } from '../../src/ts/media/portraitRendition'
import { requireImage } from '../../src/ts/media/imageFormats'

window.imageFormats = {
    async render(bytes: number[], format: 'png-card' | 'appended-charx-jpeg') {
        const source = new Uint8Array(bytes)
        const identified = requireImage(source)
        const png = await renderPortraitPng(source, format)
        const bitmap = await createImageBitmap(new Blob([new Uint8Array(png)]))
        try {
            const canvas = document.createElement('canvas')
            canvas.width = bitmap.width; canvas.height = bitmap.height
            const ctx = canvas.getContext('2d')!
            ctx.drawImage(bitmap, 0, 0)
            return { ext: identified.ext, width: bitmap.width, height: bitmap.height, pixel: [...ctx.getImageData(0, 0, 1, 1).data], png: [...png] }
        } finally { bitmap.close() }
    },
}

declare global { interface Window { imageFormats: { render(bytes: number[], format: 'png-card' | 'appended-charx-jpeg'): Promise<{ ext: string; width: number; height: number; pixel: number[]; png: number[] }> } } }
