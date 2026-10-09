import { afterEach, expect, it, vi } from 'vitest'
import { applyImageDimensionHints, observeImageDimensions } from '../imageGeometryRender'

afterEach(() => document.body.replaceChildren())

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
