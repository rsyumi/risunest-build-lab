import { getDatabase, type Database } from "../../storage/database.svelte";
import { normalizeInlayEncodeOptions } from "../../storage/blobStore";
import { ByteBudgetLru } from "../../util/byteBudgetLru";
import { getRuntimePerformanceBudgets, subscribeRuntimePerformanceProfile } from "../../runtimePerformanceProfile";
import {
    decodeInlayImageBitmap,
    encodeInlayImageWithCanvas,
    isAnimatedInlayImage,
} from "./inlayImageEncoding";

export interface InlayProviderImage {
    data: string
    width?: number
    height?: number
}

/** What every image provider reads. Anything else travels as a still WebP. */
const providerReadableMimes = new Set(['image/png', 'image/jpeg', 'image/webp'])
const maxRememberedStillFrames = 16
export const providerImagePixelBudget = 1024 * 1024
function createStillFrameCache() {
    return new ByteBudgetLru<string, InlayProviderImage>(
        getRuntimePerformanceBudgets().providerImageCacheBytes,
        (id, image) => (id.length + image.data.length) * 2 + 32,
        maxRememberedStillFrames,
    )
}
let stillFrames = createStillFrameCache()
subscribeRuntimePerformanceProfile(forgetInlayProviderImages)

function dataUriParts(dataUri: string): { mime: string, base64: string } | null {
    const match = /^data:([^;,]*)(;base64)?,(.*)$/s.exec(dataUri)
    if (!match || !match[2]) return null
    return { mime: match[1].split(';', 1)[0].trim().toLowerCase(), base64: match[3] }
}

function decodeBase64(value: string): Uint8Array {
    const binary = atob(value)
    const bytes = new Uint8Array(binary.length)
    for (let index = 0; index < binary.length; index += 1) bytes[index] = binary.charCodeAt(index)
    return bytes
}

function encodeBase64(bytes: Uint8Array): string {
    let binary = ''
    for (const byte of bytes) binary += String.fromCharCode(byte)
    return btoa(binary)
}

type ProviderImageSettings = Partial<Pick<Database,
    'risunestInlayAnimationStillFrame' | 'risunestInlayWebpQuality'>>

function providerImageSettings(): ProviderImageSettings {
    return (getDatabase() ?? {}) as ProviderImageSettings
}

function remember(id: string, image: InlayProviderImage, cache: typeof stillFrames): InlayProviderImage {
    cache.set(id, image)
    return image
}

export function forgetInlayProviderImages(): void {
    stillFrames.clear()
    stillFrames = createStillFrameCache()
}

/**
 * Turns a stored attachment into something the model can actually read: an
 * animation becomes its first scene, and a format providers do not list becomes
 * a still WebP. Whatever cannot be converted travels exactly as it is stored.
 */
export async function inlayImageForProvider(
    id: string,
    asset: InlayProviderImage,
): Promise<InlayProviderImage> {
    const settings = providerImageSettings()
    const cache = stillFrames
    const parts = dataUriParts(asset.data)
    if (!parts) return asset
    try {
        const bytes = decodeBase64(parts.base64)
        const animated = isAnimatedInlayImage(bytes)
        if (animated && settings.risunestInlayAnimationStillFrame === false) return asset
        const oversized = (asset.width ?? 0) * (asset.height ?? 0) > providerImagePixelBudget
        if (providerReadableMimes.has(parts.mime) && !animated && !oversized && asset.width && asset.height) return asset
        const digest = await crypto.subtle.digest('SHA-256', bytes as Uint8Array<ArrayBuffer>)
        const hash = Array.from(new Uint8Array(digest), byte => byte.toString(16).padStart(2, '0')).join('')
        const cacheKey = `${id}:${hash}:${settings.risunestInlayWebpQuality ?? 85}:${parts.mime}`
        const remembered = cache.get(cacheKey)
        if (remembered) return remembered
        const bitmap = await decodeInlayImageBitmap(bytes, parts.mime)
        try {
            const pixels = bitmap.width * bitmap.height
            if (!animated && providerReadableMimes.has(parts.mime) && pixels <= providerImagePixelBudget) return asset
            const maxDimension = pixels > providerImagePixelBudget
                ? Math.max(1, Math.floor(Math.max(bitmap.width, bitmap.height) * Math.sqrt(providerImagePixelBudget / pixels))) : 0
            const encoded = await encodeInlayImageWithCanvas(
                bitmap,
                bitmap.width,
                bitmap.height,
                {
                    ...normalizeInlayEncodeOptions({ quality: settings.risunestInlayWebpQuality }),
                    format: 'webp',
                    maxDimension,
                },
            )
            return remember(cacheKey, {
                data: `data:${encoded.mime};base64,${encodeBase64(encoded.data)}`,
                width: encoded.width,
                height: encoded.height,
            }, cache)
        } finally {
            bitmap.close?.()
        }
    } catch (error) {
        void error
        // Sending the stored bytes is better than dropping the attachment.
        return asset
    }
}
