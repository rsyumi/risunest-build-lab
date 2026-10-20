import { languageEnglish } from '../../src/lang/en'
import { readFileSync } from 'node:fs'
import { test, expect } from './fixture'

test('WebView decodes AVIF portraits and keeps the first frame and alpha in a PNG rendition', async ({ page }) => {
    await page.goto('/imageFormats.html')
    await page.waitForFunction(() => !!window.imageFormats)
    for (const name of ['still', 'animated']) {
        const bytes = [...readFileSync(new URL(`../fixtures/images/${name}.avif`, import.meta.url))]
        for (const format of ['png-card', 'appended-charx-jpeg'] as const) {
            const result = await page.evaluate(({ bytes, format }) => window.imageFormats.render(bytes, format), { bytes, format })
            expect(result).toMatchObject({ ext: 'avif', width: 4, height: 3 })
            expect(result.pixel[0]).toBeGreaterThan(200)
            expect(result.pixel[2]).toBeLessThan(100)
            expect(result.pixel[3]).toBeGreaterThan(150)
            expect(result.pixel[3]).toBeLessThan(170)
            expect(result.png.slice(0, 8)).toEqual([137, 80, 78, 71, 13, 10, 26, 10])
        }
    }
})

test('WebView portrait decode failures use the shared export error', async ({ page }) => {
    await page.goto('/imageFormats.html')
    await page.waitForFunction(() => !!window.imageFormats)
    const bytes = [...readFileSync(new URL('../fixtures/images/still.avif', import.meta.url))].slice(0, 32)
    const result = await page.evaluate(async bytes => {
        try { await window.imageFormats.render(bytes, 'png-card'); return null }
        catch (error) { return (error as Error).message }
    }, bytes)
    expect(result).toBe(languageEnglish.risuNest.imageFormats.portraitExportFailed)
})

test('WebView renders a legacy BMP portrait as PNG without changing its pixels', async ({ page }) => {
    const bitmap = new Uint8Array(62)
    const header = new DataView(bitmap.buffer)
    bitmap.set([0x42, 0x4d])
    for (const [offset, value] of [[2, 62], [10, 54], [14, 40], [18, 2], [22, 1], [34, 8]]) header.setUint32(offset, value, true)
    header.setUint16(26, 1, true)
    header.setUint16(28, 24, true)
    bitmap.set([0, 0, 255, 0, 255, 0, 0, 0], 54)
    await page.goto('/imageFormats.html')
    await page.waitForFunction(() => !!window.imageFormats)
    const result = await page.evaluate(bytes => window.imageFormats.render(bytes, 'png-card', true), [...bitmap])
    expect(result).toMatchObject({ ext: 'legacy', width: 2, height: 1, pixel: [255, 0, 0, 255] })
    expect(result.png.slice(0, 8)).toEqual([137, 80, 78, 71, 13, 10, 26, 10])
})
