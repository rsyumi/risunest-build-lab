import { language } from 'src/lang'

const MAX_BYTES = 50 * 1024 * 1024

export async function renderPortraitPng(data: Uint8Array, format: 'png-card' | 'appended-charx-jpeg', signal?: AbortSignal): Promise<Uint8Array> {
    signal?.throwIfAborted()
    try {
        const bitmap = await createImageBitmap(new Blob([new Uint8Array(data)], { type: 'image/avif' }))
        try {
            signal?.throwIfAborted()
            const { width, height } = bitmap
            const maxPixels = format === 'png-card' ? 40_000_000 : 16 * 1024 * 1024
            if (!width || !height || width > 8192 || height > 8192 || width * height > maxPixels) throw new Error('Portrait dimensions exceed the export limit')
            const canvas = document.createElement('canvas')
            try {
                canvas.width = width
                canvas.height = height
                const context = canvas.getContext('2d')
                if (!context) throw new Error('Canvas is unavailable')
                context.drawImage(bitmap, 0, 0)
                const blob = await new Promise<Blob>((resolve, reject) => canvas.toBlob(value => value ? resolve(value) : reject(new Error('PNG encoding failed')), 'image/png'))
                signal?.throwIfAborted()
                if (blob.size > MAX_BYTES) throw new Error('PNG rendition exceeds the export limit')
                return new Uint8Array(await blob.arrayBuffer())
            } finally { canvas.width = 0; canvas.height = 0 }
        } finally { bitmap.close() }
    } catch (error) {
        signal?.throwIfAborted()
        throw new Error(language.risuNest.imageFormats.portraitExportFailed, { cause: error })
    }
}
