import { test, expect } from './fixture'
import { languageKorean as language } from '../../src/lang/ko'

declare global { interface Window { lazyApp: { open(route: string): void; layout(mobile: boolean): void; read(): { sidebarClosing: boolean; entries: number } }; lazyImportBoundary: { arm(route: string): void; hold(route: string): void; rejectHeld(): void; attempts(): number } } }

const hosts = ['settings', 'custom', 'grid', 'presets', 'personas', 'mobile'] as const
for (const back of ['header', 'Escape', 'history'] as const) {
    test(`actual mobile chat menu opens Settings and returns through ${back}`, async ({ page }) => {
        const errors: string[] = []
        page.on('pageerror', error => errors.push(error.message))
        await page.setViewportSize({ width: 390, height: 844 })
        await page.goto('/lazyApp.html')
        await page.evaluate(() => window.lazyApp.open('mobile-chat'))
        await page.getByRole('button', { name: language.menu, exact: true }).click()
        await page.getByRole('button', { name: language.settings, exact: true }).click()
        const display = page.getByRole('button', { name: language.display, exact: true })
        await display.click()
        await expect(display).toHaveCount(0)
        const goBack = async () => {
            if (back === 'header') await page.getByRole('button', { name: language.goback, exact: true }).click()
            else if (back === 'Escape') await page.keyboard.press('Escape')
            else await page.evaluate(() => history.back())
        }
        await goBack()
        await expect(display).toBeVisible()
        await goBack()
        await expect(display).toHaveCount(0)
        await expect(page.getByRole('button', { name: language.settings, exact: true })).toBeVisible()
        await page.getByRole('button', { name: language.goback, exact: true }).click()
        await expect(page.getByRole('button', { name: language.menu, exact: true })).toBeVisible()
        expect(errors).toEqual([])
    })
}
test('actual Settings returns to the selected mobile menu after layout changes', async ({ page }) => {
    await page.setViewportSize({ width: 390, height: 844 })
    await page.goto('/lazyApp.html')
    await page.evaluate(() => window.lazyApp.open('mobile-chat'))
    await page.getByRole('button', { name: language.menu, exact: true }).click()
    await page.getByRole('button', { name: language.settings, exact: true }).click()
    const display = page.getByRole('button', { name: language.display, exact: true })
    await expect(display).toBeVisible()
    await page.setViewportSize({ width: 1200, height: 800 })
    await page.evaluate(() => window.lazyApp.layout(false))
    await expect(display).toBeVisible()
    await page.setViewportSize({ width: 390, height: 844 })
    await page.evaluate(() => window.lazyApp.layout(true))
    // The page the desktop layout opened stays open, so the menu list is hidden until Back.
    await expect(display).toHaveCount(0)
    await page.getByRole('button', { name: language.goback, exact: true }).click()
    await expect(display).toBeVisible()
    await page.keyboard.press('Escape')
    await expect(display).toHaveCount(0)
    await expect(page.getByRole('button', { name: language.settings, exact: true })).toBeVisible()
})

for (const host of hosts) {
    for (const action of ['retry', 'leave'] as const) {
        test(`actual App ${host} rejected import supports ${action}`, async ({ page }) => {
            const errors: string[] = []
            page.on('pageerror', error => errors.push(error.message))
            await page.goto('/lazyApp.html')
            await page.evaluate(route => window.lazyImportBoundary.arm(route), host)
            const open = async () => host === 'grid'
                ? page.getByRole('button', { name: 'Synthetic grid opener', exact: true }).click()
                : page.evaluate(route => window.lazyApp.open(route), host)
            await open()
            const failure = page.getByRole('alert')
            await expect(failure.getByText(host === 'settings' || host === 'mobile' ? language.risuNest.lazy.settings : language.risuNest.lazy.screen, { exact: true })).toBeVisible()
            const backLabel = host === 'mobile' ? language.goback : language.close
            await expect(failure.getByRole('button')).toHaveText([language.retry, backLabel])
            if (action === 'retry') await failure.getByRole('button', { name: language.retry, exact: true }).click()
            else {
                await failure.getByRole('button', { name: backLabel, exact: true }).click()
                await expect(failure).toHaveCount(0)
                await open()
            }
            await expect(page.locator('[data-lazy-loaded]')).toHaveCount(1)
            await expect(failure).toHaveCount(0)
            expect(await page.evaluate(() => window.lazyImportBoundary.attempts())).toBe(2)
            expect(errors).toEqual([])
        })
    }
}

test('actual MobileFooter leaving a failed Settings import allows re-entry', async ({ page }) => {
    await page.goto('/lazyApp.html')
    await page.evaluate(() => window.lazyImportBoundary.arm('mobile'))
    await page.evaluate(() => window.lazyApp.open('mobile'))
    await expect(page.getByRole('alert')).toBeVisible()
    await page.getByRole('button', { name: language.character, exact: true }).click()
    await expect(page.getByRole('alert')).toHaveCount(0)
    await page.getByRole('button', { name: language.settings, exact: true }).click()
    await expect(page.locator('[data-lazy-loaded]')).toHaveCount(1)
    expect(await page.evaluate(() => window.lazyImportBoundary.attempts())).toBe(2)
})

test('actual MobileBody ignores a prior import rejection after footer leave and re-entry', async ({ page }) => {
    const errors: string[] = []
    page.on('pageerror', error => errors.push(error.message))
    await page.goto('/lazyApp.html')
    await page.evaluate(() => { window.lazyImportBoundary.hold('mobile'); window.lazyApp.open('mobile') })
    await expect(page.getByRole('status')).toBeVisible()
    await page.getByRole('button', { name: language.character, exact: true }).click()
    await expect(page.getByRole('status')).toHaveCount(0)
    await page.getByRole('button', { name: language.settings, exact: true }).click()
    await expect(page.locator('[data-lazy-loaded]')).toHaveCount(1)
    expect(await page.evaluate(() => window.lazyImportBoundary.attempts())).toBe(2)
    await page.evaluate(() => window.lazyImportBoundary.rejectHeld())
    await expect(page.locator('[data-lazy-loaded]')).toHaveCount(1)
    await expect(page.getByRole('alert')).toHaveCount(0)
    expect(errors).toEqual([])
})

test('actual App closes the open sidebar on Back and leaves Escape to its contents', async ({ page }) => {
    await page.goto('/lazyApp.html')
    await page.evaluate(() => window.lazyApp.open('sidebar'))
    await expect(page.getByRole('button', { name: 'Synthetic grid opener', exact: true })).toBeVisible()
    await expect.poll(() => page.evaluate(() => window.lazyApp.read().entries)).toBe(1)
    await page.keyboard.press('Escape')
    expect(await page.evaluate(() => window.lazyApp.read())).toEqual({ sidebarClosing: false, entries: 1 })
    await page.evaluate(() => history.back())
    await expect.poll(() => page.evaluate(() => window.lazyApp.read().sidebarClosing)).toBe(true)
    expect(page.url()).toMatch(/\/lazyApp\.html$/)
})
