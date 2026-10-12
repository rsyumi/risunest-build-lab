const limit = 256 * 1024
const marker = '\n[Body preview truncated]'

export function previewFetchRequest(text?: string, bytes?: Uint8Array): string {
    if (text !== undefined) return text.slice(0, limit) + (text.length > limit ? marker : '')
    if (!bytes) return ''
    return new TextDecoder().decode(bytes.subarray(0, limit)) + (bytes.length > limit ? marker : '')
}

export class FetchBodyPreview {
    private decoder = new TextDecoder()
    private text = ''
    private bytes = 0
    private truncated = false
    append(chunk: Uint8Array): void {
        const length = Math.min(chunk.length, limit - this.bytes)
        if (length > 0) this.text += this.decoder.decode(chunk.subarray(0, length), { stream: true })
        this.bytes += length
        this.truncated ||= length < chunk.length
    }
    finish(): string { return this.text + this.decoder.decode() + (this.truncated ? marker : '') }
}
