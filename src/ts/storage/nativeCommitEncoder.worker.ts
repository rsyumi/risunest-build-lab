import { MAX_NATIVE_REQUEST_BYTES } from './nativePersistenceValue'

self.onmessage = ({ data }: MessageEvent<{ pages: Uint8Array[]; byteLength: number }>) => {
    try {
        if (!Number.isSafeInteger(data.byteLength) || data.byteLength <= 0 || data.byteLength > MAX_NATIVE_REQUEST_BYTES ||
            data.pages.reduce((total, page) => total + page.byteLength, 0) !== data.byteLength) throw new Error('Invalid persistence pages')
        const bytes = new Uint8Array(data.byteLength)
        let offset = 0
        for (const page of data.pages) {
            bytes.set(page, offset)
            offset += page.byteLength
        }
        self.postMessage({ bytes }, { transfer: [bytes.buffer] })
    } catch (error) {
        self.postMessage({
            error: error instanceof Error ? error.message : 'Persistence encoding failed',
        })
    }
}
