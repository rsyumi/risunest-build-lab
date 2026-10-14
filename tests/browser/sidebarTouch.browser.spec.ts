import { test, expect } from './fixture'
import type { Locator, Page } from '@playwright/test'

test.use({ hasTouch: true, isMobile: true, viewport: { width: 393, height: 852 } })

async function move(page: Page, hold: number, source: string | Locator, target: string | Locator) {
    const from = await (typeof source === 'string' ? page.getByRole('button', { name: source, exact: true }) : source).boundingBox()
    const to = await (typeof target === 'string' ? page.getByRole('button', { name: target, exact: true }) : target).boundingBox()
    const cdp = await page.context().newCDPSession(page)
    const x = from!.x + from!.width / 2
    const y = from!.y + from!.height / 2
    await cdp.send('Input.dispatchTouchEvent', { type: 'touchStart', touchPoints: [{ x, y }] })
    await page.waitForTimeout(hold)
    for (let step = 1; step <= 3; step++) {
        await cdp.send('Input.dispatchTouchEvent', { type: 'touchMove', touchPoints: [{ x, y: y + (to!.y + to!.height / 2 - y) * step / 3 }] })
        await page.waitForTimeout(35)
    }
    await cdp.send('Input.dispatchTouchEvent', { type: 'touchEnd', touchPoints: [] })
    await cdp.detach()
}

test('tap, early swipe and held drag coexist with the iOS polyfill', async ({ page }) => {
    const errors: string[] = []
    page.on('pageerror', error => errors.push(error.message))
    await page.goto('/sidebarTouch.html')
    await page.getByRole('button', { name: 'Bot 1', exact: true }).tap()
    await expect(page.getByText('sidebar-clicks:1', { exact: true })).toBeVisible()
    await page.getByRole('button', { name: 'Reset sidebar' }).tap()
    // A tap during inertial scrolling stops the fling instead of activating Reset.
    const scrollEnded = page.locator('#touch-sidebar').evaluate(list => new Promise<void>(resolve => {
        list.addEventListener('scrollend', () => resolve(), { once: true })
    }))
    await move(page, 50, 'Bot 4', 'Bot 1')
    await expect(page.getByText('sidebar-scrolled', { exact: true })).toBeVisible()
    await expect(page.getByText('sidebar-drops:0', { exact: true })).toBeVisible()
    await expect(page.getByText('sidebar-clicks:0', { exact: true })).toBeVisible()
    await scrollEnded
    await page.getByRole('button', { name: 'Reset sidebar' }).tap()
    await expect(page.getByText('sidebar-top', { exact: true })).toBeVisible()
    await move(page, 600, 'Bot 3', 'Before Bot 1')
    await expect(page.getByText('sidebar-first:3', { exact: true })).toBeVisible()
    await expect(page.getByText('sidebar-drops:1', { exact: true })).toBeVisible()
    await expect(page.getByText('sidebar-clicks:0', { exact: true })).toBeVisible()
    await expect(page.locator('[data-sidebar-drag-ghost]')).toHaveCount(0)
    expect(errors).toEqual([])
})

test('real sidebar reorders bots and preserves folder creation, entry and exit', async ({ page }) => {
    const errors: string[] = []
    page.on('pageerror', error => errors.push(error.message))
    await page.goto('/sidebarTouch.html?product')
    const bot = (id: number) => page.locator(`[data-sidebar-drag-id="bot-${id}"]`)
    const order = () => page.evaluate(() => (window as any).sidebarModel.db.characterOrder)
    await expect(bot(3)).toBeVisible()
    await move(page, 600, bot(3), page.locator('[data-sidebar-drop]').first())
    await expect.poll(async () => (await order()).slice(0, 3)).toEqual(['bot-3', 'bot-1', 'bot-2'])
    await move(page, 600, bot(3), bot(1))
    await expect.poll(async () => (await order())[0].data).toEqual(['bot-3', 'bot-1'])
    const folder = page.locator('[data-sidebar-drag-id^="folder:"]')
    await move(page, 600, bot(2), folder)
    await expect.poll(async () => (await order())[0].data).toEqual(['bot-3', 'bot-1', 'bot-2'])
    await folder.locator('.avatar').tap()
    await expect(bot(2)).toBeVisible()
    await move(page, 600, bot(2), page.locator('[data-sidebar-drop]').first())
    await expect.poll(async () => (await order())[0]).toBe('bot-2')
    await expect.poll(async () => (await order())[1].data).toEqual(['bot-3', 'bot-1'])
    await move(page, 600, folder, folder)
    await expect.poll(() => page.evaluate(() => (window as any).sidebarModel.menus)).toBe(1)
    expect(await page.evaluate(() => (window as any).sidebarModel.clicks)).toBe(0)
    expect(errors).toEqual([])
})
