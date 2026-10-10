import { test, expect } from './fixture'
import type { Locator, Page } from '@playwright/test'

async function order(page: Page, kind: string) {
    return page.evaluate(kind => {
        const db = (window as any).dragState.db
        const items = kind.startsWith('lore') ? db.characters[0].globalLore
            : kind === 'regex' ? db.globalscript : kind === 'trigger' ? db.characters[0].triggerscript : db.promptTemplate
        return items.map((item: any) => item.comment ?? item.name)
    }, kind)
}
async function open(page: Page, kind: string) {
    const errors: string[] = []
    page.on('pageerror', error => errors.push(error.message))
    await page.goto(`/dragDrop.html?kind=${kind}`)
    await expect.poll(async () => ({ mounted: await page.locator('#drag-fixture').count(), errors })).toEqual({ mounted: 1, errors: [] })
}
async function drag(source: Locator, target: Locator, bottom = false) {
    await source.scrollIntoViewIfNeeded()
    const start = await source.boundingBox()
    const box = await target.boundingBox()
    const mouse = source.page().mouse
    await mouse.move(start!.x + start!.width / 2, start!.y + start!.height / 2)
    await mouse.down()
    await mouse.move(start!.x + start!.width / 2 + 8, start!.y + start!.height / 2, { steps: 3 })
    const x = box!.x + box!.width / 2
    const y = box!.y + (bottom ? box!.height - 3 : 3)
    await mouse.move(x, y, { steps: 8 })
    await mouse.move(x, y)
    await mouse.up()
}

for (const kind of ['regex', 'trigger']) {
    test(`${kind}: restores dragging after replacing an open list`, async ({ page }) => {
        await open(page, kind)
        await page.getByRole('button', { name: 'First', exact: true }).click()
        await page.evaluate(kind => {
            const db = (window as any).dragState.db
            if (kind === 'regex') db.globalscript = db.globalscript.slice(1)
            else db.characters[0].triggerscript = db.characters[0].triggerscript.slice(1)
        }, kind)
        await expect(page.getByRole('button', { name: 'First', exact: true })).toHaveCount(0)
        await drag(page.getByRole('button', { name: 'Last', exact: true }), page.getByRole('button', { name: 'Second', exact: true }))
        await expect.poll(() => order(page, kind)).toEqual(['Last', 'Second'])
    })

    test(`${kind}: delete, edit, and repeatedly reorder with the real Sortable library`, async ({ page }) => {
        const errors: string[] = []
        page.on('pageerror', error => errors.push(error.message))
        await open(page, kind)
        await page.getByRole('button', { name: 'First', exact: true }).locator('..').locator('button').nth(1).click()
        await page.getByRole('button', { name: 'Second', exact: true }).click()
        await page.getByRole('button', { name: 'Second', exact: true }).click()
        const last = page.getByRole('button', { name: 'Last', exact: true })
        const second = page.getByRole('button', { name: 'Second', exact: true })
        await drag(last, second)
        await expect.poll(() => order(page, kind)).toEqual(['Last', 'Second'])
        await drag(second, last)
        await expect.poll(() => order(page, kind)).toEqual(['Second', 'Last'])
        expect(errors).toEqual([])
    })
}

test('lore: reorder around an expanded folder without remounting its editors', async ({ page }) => {
    const errors: string[] = []
    page.on('pageerror', error => errors.push(error.message))
    await open(page, 'lore')
    await page.getByRole('button', { name: 'Folder', exact: true }).click()
    await page.getByRole('button', { name: 'Child', exact: true }).click()
    const input = await page.locator('[data-show-folder="folder"] input').first().elementHandle()
    await drag(page.getByRole('button', { name: 'Last', exact: true }), page.getByRole('button', { name: 'First', exact: true }))
    await expect.poll(() => order(page, 'lore')).toEqual(['Folder', 'Child', 'Last', 'First'])
    expect(await input!.evaluate(node => node.isConnected)).toBe(true)
    expect(errors).toEqual([])
})

test('lore: move into a folder and back to the root', async ({ page }) => {
    const errors: string[] = []
    page.on('pageerror', error => errors.push(error.message))
    await open(page, 'lore')
    await page.getByRole('button', { name: 'Folder', exact: true }).click()
    const first = page.getByRole('button', { name: 'First', exact: true })
    await drag(first, page.getByRole('button', { name: 'Child', exact: true }), true)
    const folderOfFirst = () => page.evaluate(() => (window as any).dragState.db.characters[0].globalLore.find((item: any) => item.comment === 'First').folder)
    await expect.poll(folderOfFirst).toBe('folder')
    await expect(page.locator('[data-show-folder="folder"]').getByRole('button', { name: 'First', exact: true })).toBeVisible()
    await drag(first, page.getByRole('button', { name: 'Last', exact: true }), true)
    await expect.poll(folderOfFirst).toBeUndefined()
    expect(errors).toEqual([])
})

test('prompt: use gaps and card halves without changing the drag source during hover', async ({ page }) => {
    const errors: string[] = []
    page.on('pageerror', error => errors.push(error.message))
    await open(page, 'prompt')
    const header = (name: string) => page.locator('[draggable="true"]').filter({ has: page.locator('span', { hasText: new RegExp(`^${name}$`) }) })
    const first = header('First')
    await first.click()
    const input = await first.locator('..').locator('input').first().elementHandle()
    await drag(first, header('Last'), true)
    await expect.poll(() => order(page, 'prompt')).toEqual(['Second', 'Last', 'First'])
    expect(await input!.evaluate(node => node.isConnected)).toBe(true)
    await drag(header('First'), page.locator('[role="doc-pagebreak"]').first())
    await expect.poll(() => order(page, 'prompt')).toEqual(['First', 'Second', 'Last'])
    expect(errors).toEqual([])
})

test.describe('touch dragging', () => {
    test.use({ hasTouch: true, isMobile: true, viewport: { width: 393, height: 852 } })
    test('reorders regex entries after the touch hold delay', async ({ page, context }) => {
        await open(page, 'regex')
        const source = await page.getByRole('button', { name: 'Last', exact: true }).boundingBox()
        const target = await page.getByRole('button', { name: 'First', exact: true }).boundingBox()
        const cdp = await context.newCDPSession(page)
        const x = source!.x + source!.width / 2
        const y = source!.y + source!.height / 2
        await cdp.send('Input.dispatchTouchEvent', { type: 'touchStart', touchPoints: [{ x, y }] })
        await page.waitForTimeout(350)
        for (let step = 1; step <= 6; step++) {
            await cdp.send('Input.dispatchTouchEvent', { type: 'touchMove', touchPoints: [{
                x, y: y + (target!.y + 3 - y) * step / 6,
            }] })
            await page.waitForTimeout(40)
        }
        await cdp.send('Input.dispatchTouchEvent', { type: 'touchEnd', touchPoints: [] })
        await expect.poll(() => order(page, 'regex')).toEqual(['Last', 'First', 'Second'])
    })
})

async function center(locator: Locator) {
    await expect(locator).toBeVisible()
    const box = await locator.boundingBox()
    return { x: box!.x + box!.width / 2, y: box!.y + box!.height / 2, top: box!.y + 3 }
}
const zone = (page: Page, direction: 'previous' | 'next') => page.locator(`[data-lore-page-zone="${direction}"]`)
const pagerText = (page: Page) => page.getByRole('navigation', { name: 'Pages' }).first()
const itemButton = (page: Page, name: string) => page.getByRole('button', { name, exact: true })
async function position(page: Page, name: string) {
    return (await order(page, 'lore')).indexOf(name)
}

/** Hovers the top edge of a row at a pace Sortable's move animation keeps up with. */
async function settleOn(page: Page, target: string, via?: string) {
    // Leave the repeat zone before waiting for the drop target to settle.
    if (via) {
        const pass = await center(itemButton(page, via).locator('..'))
        await page.mouse.move(pass.x, pass.y, { steps: 6 })
        // A delayed drop must stay on this page past the 700 ms repeat interval.
        await page.waitForTimeout(750)
    }
    // Rows shift once the dragged row leaves its pinned edge, so measure the target after that.
    const drop = await center(itemButton(page, target).locator('..'))
    await page.mouse.move(drop.x, drop.top, { steps: 6 })
    await page.waitForTimeout(250)
    await page.mouse.move(drop.x, drop.top)
}

test.describe('lore pages', () => {
    test.use({ viewport: { width: 800, height: 6400 } })

    async function dragThroughZone(page: Page, source: string, direction: 'previous' | 'next', expected: string, target: string, via?: string) {
        const start = await center(itemButton(page, source).locator('..'))
        const mouse = page.mouse
        await mouse.move(start.x, start.y)
        await mouse.down()
        await mouse.move(start.x + 8, start.y, { steps: 3 })
        // The zone moves when the dragged row enters this list, so aim again once it settles.
        for (let attempt = 0; attempt < 2; attempt++) {
            const edge = await center(zone(page, direction))
            await mouse.move(edge.x, edge.y, { steps: 8 })
            await page.waitForTimeout(200)
        }
        await expect(pagerText(page)).toContainText(expected)
        await settleOn(page, target, via)
        await expect(pagerText(page)).toContainText(expected)
        await mouse.up()
    }

    test('hovering the next-page zone carries a row to a later page', async ({ page }) => {
        const errors: string[] = []
        page.on('pageerror', error => errors.push(error.message))
        await open(page, 'lore-pages')
        await expect(zone(page, 'next')).toHaveCount(0)
        await dragThroughZone(page, 'Item 0', 'next', '2 / 3', 'Item 70', 'Item 100')
        await expect.poll(async () => (await position(page, 'Item 70')) - (await position(page, 'Item 0'))).toBe(1)
        await expect(itemButton(page, 'Item 0')).toBeVisible()
        await expect(zone(page, 'next')).toHaveCount(0)
        expect(errors).toEqual([])
    })

    test('hovering the previous-page zone carries a row to an earlier page', async ({ page }) => {
        const errors: string[] = []
        page.on('pageerror', error => errors.push(error.message))
        await open(page, 'lore-pages')
        await page.getByRole('button', { name: 'Next page' }).first().click()
        await dragThroughZone(page, 'Item 100', 'previous', '1 / 3', 'Item 5')
        await expect.poll(async () => (await position(page, 'Item 5')) - (await position(page, 'Item 100'))).toBe(1)
        await expect(itemButton(page, 'Item 100')).toBeVisible()
        expect(errors).toEqual([])
    })

    test('a row dragged out of a folder survives the root page change', async ({ page }) => {
        const errors: string[] = []
        page.on('pageerror', error => errors.push(error.message))
        await open(page, 'lore-pages')
        await itemButton(page, 'Folder').click()
        await dragThroughZone(page, 'Child', 'next', '2 / 3', 'Item 70', 'Item 100')
        const child = () => page.evaluate(() => (window as any).dragState.db.characters[0].globalLore.find((item: any) => item.comment === 'Child').folder)
        await expect.poll(child).toBeUndefined()
        await expect.poll(async () => (await position(page, 'Item 70')) - (await position(page, 'Child'))).toBe(1)
        await expect(itemButton(page, 'Child')).toBeVisible()
        expect(errors).toEqual([])
    })
})

test.describe('lore pages by touch', () => {
    test.use({ hasTouch: true, isMobile: true, viewport: { width: 393, height: 6400 } })
    test('holding a touch drag on the next-page zone carries a row to a later page', async ({ page, context }) => {
        const errors: string[] = []
        page.on('pageerror', error => errors.push(error.message))
        await open(page, 'lore-pages')
        const start = await center(itemButton(page, 'Item 0'))
        const cdp = await context.newCDPSession(page)
        const touch = (type: 'touchStart' | 'touchEnd' | 'touchMove' | 'touchCancel', x: number, y: number) =>
            cdp.send('Input.dispatchTouchEvent', { type, touchPoints: type === 'touchEnd' ? [] : [{ x, y }] })
        await touch('touchStart', start.x, start.y)
        await page.waitForTimeout(350)
        await touch('touchMove', start.x, start.y + 10)
        await expect(zone(page, 'next')).toHaveCount(1)
        const edge = await center(zone(page, 'next'))
        for (let step = 1; step <= 6; step++) {
            await touch('touchMove', edge.x, start.y + (edge.y - start.y) * step / 6)
            await page.waitForTimeout(40)
        }
        await expect(pagerText(page)).toContainText('2 / 3')
        await page.waitForTimeout(250)
        const pass = await center(itemButton(page, 'Item 100'))
        for (let step = 1; step <= 6; step++) {
            await touch('touchMove', pass.x, edge.y + (pass.y - edge.y) * step / 6)
            await page.waitForTimeout(60)
        }
        await page.waitForTimeout(250)
        const drop = await center(itemButton(page, 'Item 70'))
        for (let step = 1; step <= 6; step++) {
            await touch('touchMove', drop.x, pass.y + (drop.top - pass.y) * step / 6)
            await page.waitForTimeout(60)
        }
        await page.waitForTimeout(250)
        await touch('touchEnd', 0, 0)
        await expect.poll(async () => (await position(page, 'Item 70')) - (await position(page, 'Item 0'))).toBe(1)
        expect(errors).toEqual([])
    })
})
