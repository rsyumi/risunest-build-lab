import { validImageDimensions } from '../../storage/imageGeometry'

interface ImageSource {
    url: string
    width?: number
    height?: number
    recordDimensions?: (width: number, height: number) => Promise<void>
}

export function applyImageDimensionHints(element: HTMLImageElement, source: ImageSource): void {
    if (element.hasAttribute('width') || element.hasAttribute('height') || element.hasAttribute('srcset') ||
        element.style.width || element.style.height || element.closest('picture')?.querySelector('source[srcset]') ||
        !validImageDimensions(source.width, source.height)) return
    element.setAttribute('width', String(source.width))
    element.setAttribute('height', String(source.height))
}

export function observeImageDimensions(element: HTMLImageElement, source: ImageSource, isCurrent: () => boolean): () => void {
    if (!source.recordDimensions || validImageDimensions(source.width, source.height)) return () => {}
    const ready = () => {
        if (!isCurrent() || !element.isConnected || element.hasAttribute('srcset') ||
            element.closest('picture')?.querySelector('source[srcset]') || element.getAttribute('src') !== source.url ||
            !validImageDimensions(element.naturalWidth, element.naturalHeight)) return
        element.removeEventListener('load', ready)
        void source.recordDimensions!(element.naturalWidth, element.naturalHeight)
            .catch(() => console.warn('Image dimensions could not be saved'))
    }
    element.addEventListener('load', ready)
    if (element.complete) ready()
    return () => element.removeEventListener('load', ready)
}
