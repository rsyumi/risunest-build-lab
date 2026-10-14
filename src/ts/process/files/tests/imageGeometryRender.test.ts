import { afterEach, expect, it, vi } from 'vitest'
import { applyImageDimensionHints, bindImageReservations, imageReservationAttributes, observeImageDimensions } from '../imageGeometryRender'

afterEach(() => { document.body.replaceChildren(); vi.restoreAllMocks(); vi.unstubAllGlobals() })

function image() {
    const element = document.createElement('img')
    element.src = 'https://synthetic.invalid/image'
    Object.defineProperties(element, { naturalWidth: { value: 640 }, naturalHeight: { value: 480 }, complete: { value: false } })
    document.body.append(element)
    return element
}

it('learns natural dimensions once against the resolved source, independent of CSS size', async () => {
    const element = image()
    element.style.width = '40px'
    const recordDimensions = vi.fn(async () => {})
    const cleanup = observeImageDimensions(element, { url: element.src, recordDimensions }, () => true)
    element.dispatchEvent(new Event('load'))
    element.dispatchEvent(new Event('load'))
    expect(recordDimensions).toHaveBeenCalledExactlyOnceWith(640, 480)
    cleanup()
})

it.each(['replaced', 'responsive', 'picture', 'disposed', 'unmounted'] as const)('ignores %s image readiness', mode => {
    const element = image()
    const source = { url: element.src, recordDimensions: vi.fn(async () => {}) }
    const cleanup = observeImageDimensions(element, source, () => mode !== 'disposed')
    if (mode === 'replaced') element.src += '?new'
    if (mode === 'responsive') element.srcset = 'https://synthetic.invalid/2x 2x'
    if (mode === 'picture') {
        const picture = document.createElement('picture')
        picture.innerHTML = '<source srcset="https://synthetic.invalid/other">'
        document.body.append(picture)
        picture.append(element)
    }
    if (mode === 'unmounted') element.remove()
    element.dispatchEvent(new Event('load'))
    expect(source.recordDimensions).not.toHaveBeenCalled()
    cleanup()
})

it('adds intrinsic hints while preserving authored sizing', () => {
    const element = image()
    applyImageDimensionHints(element, { url: element.src, width: 640, height: 480 })
    expect([element.width, element.height]).toEqual([640, 480])
    const authored = image()
    authored.setAttribute('width', '100')
    applyImageDimensionHints(authored, { url: authored.src, width: 640, height: 480 })
    expect(authored.getAttribute('width')).toBe('100')
    expect(authored.hasAttribute('height')).toBe(false)
})

function enableReservations() {
    vi.stubGlobal('CSS', { supports: vi.fn(() => true) })
    vi.spyOn(globalThis, 'getComputedStyle').mockReturnValue({
        contain: 'none', contentVisibility: 'visible', containIntrinsicWidth: 'none', containIntrinsicHeight: 'none',
    } as CSSStyleDeclaration)
}

it('keeps a deferred reservation through its owned object URL assignment, then releases on replacement', async () => {
    enableReservations()
    const element = image()
    element.removeAttribute('src')
    const source = { url: 'blob:old-render', width: 600, height: 900 }
    applyImageDimensionHints(element, source)
    let url = source.url
    const cleanup = bindImageReservations([[element, { source }]], () => url)
    expect(element.hasAttribute('data-risu-image-size')).toBe(true)
    url = 'blob:current-render'
    element.src = url
    await new Promise(resolve => setTimeout(resolve, 0))
    expect(element.hasAttribute('data-risu-image-size')).toBe(true)
    element.removeAttribute('src')
    await new Promise(resolve => setTimeout(resolve, 0))
    expect(element.hasAttribute('data-risu-image-size')).toBe(false)
    cleanup()
})

it.each(['load', 'error', 'cleanup'] as const)('releases image reservations on %s without reverting plugin styles', action => {
    enableReservations()
    const element = image(), source = { url: element.src, width: 640, height: 480 }
    applyImageDimensionHints(element, source)
    const cleanup = bindImageReservations([[element, { source }]])
    element.style.width = '75px'
    element.style.setProperty('--risu-image-width', '777px')
    if (action === 'cleanup') cleanup()
    else element.dispatchEvent(new Event(action))
    expect(element.hasAttribute('data-risu-image-size')).toBe(false)
    expect(element.style.getPropertyValue('--risu-image-height')).toBe('')
    expect(element.style.width).toBe('75px')
    expect(element.style.getPropertyValue('--risu-image-width')).toBe('777px')
    cleanup()
    element.dispatchEvent(new Event('load'))
    expect(element.style.getPropertyValue('--risu-image-width')).toBe('777px')
})

it('excludes unsupported sizing and invalid generated dimensions', () => {
    vi.stubGlobal('CSS', { supports: vi.fn(() => false) })
    expect(imageReservationAttributes(640, 480)).toBe('')
    const element = image()
    applyImageDimensionHints(element, { url: element.src, width: 640, height: 480 })
    expect(element.hasAttribute('data-risu-image-size')).toBe(false)
    vi.mocked(CSS.supports).mockReturnValue(true)
    for (const value of [0, -1, 0.5, Infinity, 0x1_0000_0000]) expect(imageReservationAttributes(value, 480)).toBe('')
})
