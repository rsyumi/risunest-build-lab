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
    prepareImageReservation(element, source.width!, source.height!)
}

const reservationAttribute = 'data-risu-image-size'
const widthProperty = '--risu-image-width'
const heightProperty = '--risu-image-height'

function supportsImageReservation(): boolean {
    return typeof CSS !== 'undefined' && CSS.supports('contain', 'size') &&
        CSS.supports('contain-intrinsic-size', '1px 1px')
}

export function imageReservationAttributes(width?: number, height?: number): string {
    if (!supportsImageReservation() || !validImageDimensions(width, height)) return ''
    return ` ${reservationAttribute}="${width} ${height}" style="${widthProperty}:${width}px;${heightProperty}:${height}px"`
}

function prepareImageReservation(element: HTMLImageElement, width: number, height: number): void {
    if (!supportsImageReservation() || element.style.contain || element.style.containIntrinsicSize ||
        element.style.contentVisibility || element.hasAttribute(reservationAttribute) ||
        element.style.getPropertyValue(widthProperty) || element.style.getPropertyValue(heightProperty)) return
    element.setAttribute(reservationAttribute, `${width} ${height}`)
    element.style.setProperty(widthProperty, `${width}px`)
    element.style.setProperty(heightProperty, `${height}px`)
}

export function bindImageReservations(
    entries: Iterable<[HTMLElement, { source: ImageSource }]>,
    currentUrl: (element: HTMLImageElement, source: ImageSource) => string = (_element, source) => source.url,
): () => void {
    const candidates: Array<{ element: HTMLImageElement, source: ImageSource, value: string, width: string, height: string }> = []
    for (const [element, { source }] of entries) {
        if (!(element instanceof HTMLImageElement) || !validImageDimensions(source.width, source.height)) continue
        const value = element.getAttribute(reservationAttribute)
        const width = `${source.width}px`, height = `${source.height}px`
        if (value !== `${source.width} ${source.height}` ||
            element.style.getPropertyValue(widthProperty) !== width ||
            element.style.getPropertyValue(heightProperty) !== height) continue
        candidates.push({ element, source, value, width, height })
        element.removeAttribute(reservationAttribute)
    }
    // Batch reads while our rule is inactive so authored containment wins.
    const eligible = candidates.map(({ element }) => {
        const style = getComputedStyle(element)
        return supportsImageReservation() && style.contain === 'none' && style.contentVisibility === 'visible' &&
            style.containIntrinsicWidth === 'none' && style.containIntrinsicHeight === 'none' &&
            !element.hasAttribute('srcset') && !element.closest('picture')?.querySelector('source[srcset]')
    })
    const active = new Map<HTMLImageElement, () => void>()
    const cleanups: Array<() => void> = []
    const observer = typeof MutationObserver === 'undefined' ? null : new MutationObserver(records => {
        for (const record of records) active.get(record.target as HTMLImageElement)?.()
    })
    candidates.forEach(({ element, source, value, width, height }, index) => {
        let disposed = false
        const remove = () => {
            if (disposed) return
            disposed = true
            active.delete(element)
            if (!active.size) observer?.disconnect()
            element.removeEventListener('load', remove)
            element.removeEventListener('error', remove)
            if (element.getAttribute(reservationAttribute) === value) element.removeAttribute(reservationAttribute)
            if (element.style.getPropertyValue(widthProperty) === width) element.style.removeProperty(widthProperty)
            if (element.style.getPropertyValue(heightProperty) === height) element.style.removeProperty(heightProperty)
        }
        cleanups.push(remove)
        const url = element.getAttribute('src')
        if (!eligible[index] || (url && url !== currentUrl(element, source)) ||
            (url && element.complete && element.naturalWidth > 0)) { remove(); return }
        element.setAttribute(reservationAttribute, value)
        element.addEventListener('load', remove)
        element.addEventListener('error', remove)
        active.set(element, () => {
            if (element.getAttribute('src') !== currentUrl(element, source) || element.hasAttribute('srcset')) remove()
        })
    })
    for (const element of active.keys()) observer?.observe(element, { attributes: true, attributeFilter: ['src', 'srcset'] })
    return () => { observer?.disconnect(); for (const cleanup of cleanups) cleanup() }
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
