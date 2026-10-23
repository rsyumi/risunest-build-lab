import { test, expect } from './fixture'
import { languageKorean as language } from '../../src/lang/ko'

declare global { interface Window { lazyApp: { open(route: string): void; layout(mobile: boolean): void; read(): { sidebarClosing: boolean; entries: number }; lock: { initialize(): Promise<void>; bootFailure(loaded: boolean): void; requestExit(): void } }; lazyImportBoundary: { arm(route: string): void; hold(route: string): void; rejectHeld(): void; attempts(): number } } }

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


test.describe('actual App lock integration', () => {
    test.beforeEach(async ({ page }) => {
        await page.addInitScript(() => {
            let callback = 0
            Object.assign(window, {
                __TAURI_INTERNALS__: {
                    transformCallback: () => ++callback,
                    invoke: async (command: string) => {
                        if (command === 'app_lock_check') return { auth: { status: 'unsupported' }, previewSupported: false }
                        if (command === 'app_lock_set_policy') return { active: true, epoch: 0, atMs: 0, departedAtMs: null }
                        if (command === 'app_lock_ready') return true
                        if (command === 'plugin:event|listen') return 1
                        if (command === 'diagnostic_device_info') return { model: 'Synthetic', arch: 'synthetic' }
                        throw new Error(`Unexpected synthetic native command: ${command}`)
                    },
                },
            })
        })
        await page.goto('/lazyApp.html')
    })

    test('retains the mounted draft and restores focus after PIN unlock despite a post-load boot failure', async ({ page }) => {
        const errors: string[] = []
        page.on('pageerror', error => errors.push(error.message))
        const draft = page.getByRole('textbox', { name: 'Synthetic retained draft' })
        await draft.fill('Retained draft')
        await draft.evaluate(node => { node.dataset.retained = 'original' })
        await page.evaluate(() => window.lazyApp.lock.initialize())
        const gate = page.getByRole('dialog', { name: 'RisuNest' })
        await expect(gate).toBeVisible()
        await expect(page.locator('main')).toHaveAttribute('inert', '')
        await page.evaluate(() => window.lazyApp.lock.bootFailure(true))
        await expect(gate).toBeVisible()
        await expect(page.getByRole('heading', { name: language.risuNest.boot.title })).toHaveCount(0)
        await expect(page.locator('main')).toHaveAttribute('inert', '')
        await page.keyboard.type('123456')
        await expect(gate).toHaveCount(0)
        await expect(draft).toBeFocused()
        await expect(draft).toHaveValue('Retained draft')
        await expect(draft).toHaveAttribute('data-retained', 'original')
        await page.keyboard.type(' preserved')
        await expect(draft).toHaveValue('Retained draft preserved')
        expect(errors).toEqual([])
    })

    test('shows the startup failure panel without a PIN gate when normal content never loaded', async ({ page }) => {
        await page.evaluate(() => window.lazyApp.lock.initialize())
        await page.evaluate(() => window.lazyApp.lock.bootFailure(false))
        await expect(page.getByRole('heading', { name: language.risuNest.boot.title })).toBeVisible()
        await expect(page.getByRole('dialog', { name: 'RisuNest' })).toHaveCount(0)
        await expect(page.locator('main')).not.toHaveAttribute('inert')
    })

    test('tabs through visible exit actions and returns focus to the locked screen on cancel', async ({ page }) => {
        await page.evaluate(() => window.lazyApp.lock.initialize())
        const gate = page.getByRole('dialog', { name: 'RisuNest' })
        await expect(gate).toBeVisible()
        await page.evaluate(() => window.lazyApp.lock.requestExit())
        const exit = page.getByTestId('sync-exit-dialog')
        await expect(exit).toBeFocused()
        const summary = exit.locator('summary')
        await page.keyboard.press('Tab')
        await expect(summary).toBeFocused()
        await page.keyboard.press('Tab')
        await expect(exit.getByRole('button', { name: language.risuNest.exitDrain.cancelExit, exact: true })).toBeFocused()
        await page.keyboard.press('Shift+Tab')
        await page.keyboard.press('Enter')
        await expect(exit.locator('details')).toHaveAttribute('open', '')
        await page.keyboard.press('Tab')
        await expect(exit.getByRole('button', { name: language.copy, exact: true })).toBeFocused()
        await page.keyboard.press('Escape')
        await expect(exit).toHaveCount(0)
        await expect(gate).toBeFocused()
        await expect(page.locator('main')).toHaveAttribute('inert', '')
        await page.keyboard.type('123456')
        await expect(gate).toHaveCount(0)
    })
})
