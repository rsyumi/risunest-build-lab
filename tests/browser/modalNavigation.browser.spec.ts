import { test, expect } from './fixture'
import type { NavigationDriver } from './modalNavigationMain'
declare global { interface Window { modalHistory: NavigationDriver } }

const state = (page: import('@playwright/test').Page) => page.evaluate(() => window.modalHistory.state())
test.beforeEach(async ({ page }) => { await page.goto('/modalNavigation.html') })

test('destroying nested modals together removes both history entries and restores the opener', async ({ page }) => {
    await page.evaluate(() => window.modalHistory.open(true))
    await expect(page.locator('#child-control')).toBeFocused()
    await page.evaluate(() => window.modalHistory.destroyNested())
    await expect.poll(async () => (await state(page)).state).toEqual({ page: 'baseline' })
    await expect(page.locator('#opener')).toBeFocused()
    expect((await state(page)).closed).toEqual({ parent: 0, child: 0 })
    await page.evaluate(() => history.back())
    await expect.poll(async () => (await state(page)).hash).toBe('#before')
    expect((await state(page)).pops).toBe(2)
})

test('Back across a foreign entry preserves the modal until its own token is removed', async ({ page }) => {
    await page.evaluate(() => window.modalHistory.open())
    await expect(page.locator('#parent-control')).toBeFocused()
    await page.evaluate(() => { window.modalHistory.pushForeign(); history.back() })
    await expect.poll(async () => (await state(page)).pops).toBe(1)
    expect((await state(page)).closed.parent).toBe(0)
    expect((await state(page)).parentConnected).toBe(true)
    await page.evaluate(() => history.back())
    await expect(page.locator('#parent')).toHaveCount(0)
    await expect(page.locator('#opener')).toBeFocused()
    expect((await state(page)).closed.parent).toBe(1)
    await page.evaluate(() => history.back())
    await expect.poll(async () => (await state(page)).hash).toBe('#before')
    expect((await state(page)).closed.parent).toBe(1)
})

test('Back cancels an alert before closing its host and restores host focus', async ({ page }) => {
    await page.evaluate(() => window.modalHistory.open())
    await expect(page.locator('#parent-control')).toBeFocused()
    await page.evaluate(() => { window.modalHistory.showAlert(); history.back() })
    await expect.poll(async () => (await state(page)).alert).toBe('none')
    await expect(page.locator('#parent-control')).toBeFocused()
    expect((await state(page)).closed.parent).toBe(0)
    await page.evaluate(() => history.back())
    await expect(page.locator('#parent')).toHaveCount(0)
    await expect(page.locator('#opener')).toBeFocused()
    expect((await state(page)).closed.parent).toBe(1)
})
