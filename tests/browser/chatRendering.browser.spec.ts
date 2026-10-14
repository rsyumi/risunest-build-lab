import { test, expect } from './fixture'
import type {} from './chatRenderingMain'

test('managed auto images reserve their final size and release only owned styles', async ({ page }) => {
    let release!: () => void
    const loading = new Promise<void>(resolve => { release = resolve })
    await page.route('**/synthetic-image-*', async route => {
        await loading
        if (route.request().url().endsWith('error')) return route.abort()
        await route.fulfill({ contentType: 'image/svg+xml', body: '<svg xmlns="http://www.w3.org/2000/svg" width="600" height="900"/>' })
    })
    await page.goto('/chatRendering.html')
    const before = await page.evaluate(() => {
        const style = document.createElement('style')
        style.textContent = '#fixture img { width:auto; height:auto; max-width:100%; max-height:480px } .authored { contain:paint }'
        document.head.append(style)
        const tests = ['auto', 'resize', 'hidden', 'authored', 'error', 'plugin', 'removed', 'replaced', 'cleanup'].map(name => {
            const host = document.createElement('div')
            host.style.width = '800px'
            const img = document.createElement('img')
            img.id = name
            if (name === 'hidden') img.style.display = 'none'
            if (name === 'authored') img.className = 'authored'
            const source = { url: `${location.origin}/synthetic-image-${name}`, width: 600, height: 900 }
            img.src = source.url
            window.chatRendering.applyImageDimensionHints(img, source)
            host.append(img)
            document.querySelector('#fixture')!.append(host)
            const cleanup = window.chatRendering.bindImageReservations([[img, { source }]])
            if (name === 'resize') host.style.width = '240px'
            const rect = img.getBoundingClientRect()
            if (name === 'plugin') { img.style.width = '72px'; img.style.setProperty('--risu-image-width', '777px') }
            if (name === 'removed') img.removeAttribute('src')
            if (name === 'replaced') img.src = 'data:image/svg+xml,<svg xmlns="http://www.w3.org/2000/svg" width="100" height="50"/>'
            if (name === 'cleanup') cleanup()
            return { name, size: [rect.width, rect.height], reserved: img.hasAttribute('data-risu-image-size') }
        })
        return tests
    })
    expect(before.find(row => row.name === 'auto')!.size).toEqual([320, 480])
    expect(before.find(row => row.name === 'resize')!.size).toEqual([240, 360])
    expect(before.find(row => row.name === 'authored')!.reserved).toBe(false)
    await expect(page.locator('#removed')).not.toHaveAttribute('data-risu-image-size')
    release()
    await expect.poll(() => page.evaluate(() => [...document.querySelectorAll('img')].every(img => img.complete))).toBe(true)
    await expect(page.locator('img[data-risu-image-size]')).toHaveCount(0)
    for (const name of ['auto', 'resize', 'hidden']) {
        const size = await page.locator(`#${name}`).evaluate(img => { const r = img.getBoundingClientRect(); return [r.width, r.height] })
        expect(size).toEqual(before.find(row => row.name === name)!.size)
    }
    expect(await page.locator('#plugin').evaluate(img => [(img as HTMLElement).style.width, (img as HTMLElement).style.getPropertyValue('--risu-image-width')])).toEqual(['72px', '777px'])
    expect(await page.locator('#authored').evaluate(img => getComputedStyle(img).contain)).toBe('paint')
    expect(await page.locator('#replaced').evaluate(img => (img as HTMLImageElement).naturalWidth)).toBe(100)
})

test('replacing tall rows with gaps preserves range, retained controls and their screen position', async ({ page }) => {
    await page.goto('/chatRendering.html')
    const result = await page.evaluate(async () => {
        document.querySelector('#fixture')!.innerHTML = '<div id="scroller" style="height:800px;width:600px;overflow:auto;display:flex;flex-direction:column-reverse;overflow-anchor:none"><div id="rows" style="display:flex;flex-direction:column-reverse;flex:none"><div style="height:18000px;flex:none"></div><div id="removed" style="height:24000px;flex:none"></div><div id="anchor" style="height:20000px;flex:none"><button>Retained control</button></div><div style="height:16000px;flex:none"></div></div></div>'
        const container = document.querySelector<HTMLElement>('#scroller')!, rows = document.querySelector<HTMLElement>('#rows')!
        const anchor = document.querySelector<HTMLElement>('#anchor')!, removed = document.querySelector<HTMLElement>('#removed')!
        const frame = () => new Promise<void>(resolve => requestAnimationFrame(() => resolve()))
        container.scrollTop = anchor.getBoundingClientRect().top - container.getBoundingClientRect().top + 1200
        anchor.querySelector('button')!.focus({ preventScroll: true })
        await frame()
        const read = () => ({ top: container.scrollTop, height: container.scrollHeight, offset: anchor.getBoundingClientRect().top - container.getBoundingClientRect().top })
        const before = read(), observations: ReturnType<typeof read>[] = []
        const gap = document.createElement('div'); gap.style.cssText = 'height:24000px;flex:none'
        const remove = removed.remove.bind(removed)
        removed.remove = () => { remove(); observations.push(read()) }
        window.chatRendering.reconcileChatViewportChildren(rows, [...rows.children].map(row => row === removed ? gap : row) as HTMLElement[])
        await frame()
        return { before, after: read(), observations, focus: document.activeElement === anchor.querySelector('button') }
    })
    expect(result.after).toEqual(result.before)
    expect(result.observations).toEqual([result.before])
    expect(result.focus).toBe(true)
})

test('cached, authored, responsive and unsupported images retain their own sizing', async ({ page }) => {
    await page.goto('/chatRendering.html')
    const results = await page.evaluate(async () => {
        const { applyImageDimensionHints: apply, bindImageReservations: bind } = window.chatRendering
        const url = 'data:image/svg+xml,<svg xmlns="http://www.w3.org/2000/svg" width="600" height="900"/>'
        const source = { url, width: 600, height: 900 }
        const cached = document.createElement('img'); cached.src = url
        document.querySelector('#fixture')!.append(cached)
        await cached.decode()
        apply(cached, source)
        const cleanup = bind([[cached, { source }]])
        const cachedReleased = !cached.hasAttribute('data-risu-image-size')
        cleanup()
        cached.style.setProperty('--risu-image-width', '77px')
        cached.dispatchEvent(new Event('load'))
        const excluded = ['width', 'style', 'srcset', 'picture'].map(kind => {
            const img = document.createElement('img')
            if (kind === 'width') img.setAttribute('width', '72')
            if (kind === 'style') img.style.height = '90px'
            if (kind === 'srcset') img.srcset = `${url} 1x`
            if (kind === 'picture') {
                const picture = document.createElement('picture'), alternative = document.createElement('source')
                alternative.srcset = `${url} 1x`; picture.append(alternative, img)
            }
            apply(img, source)
            return !img.hasAttribute('data-risu-image-size')
        })
        const supports = CSS.supports, unsupported = document.createElement('img')
        try { CSS.supports = () => false; apply(unsupported, source) } finally { CSS.supports = supports }
        return { cachedReleased, staleLoad: cached.style.getPropertyValue('--risu-image-width'), excluded, unsupported: !unsupported.hasAttribute('data-risu-image-size') }
    })
    expect(results).toEqual({ cachedReleased: true, staleLoad: '77px', excluded: [true, true, true, true], unsupported: true })
})
