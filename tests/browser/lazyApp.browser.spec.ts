import { test, expect } from './fixture'

declare global { interface Window { lazyApp: { open(route: string): void; showDialog(): void; read(): { sidebarClosing: boolean; dialog: string; entries: number } }; lazyImportBoundary: { arm(route: string): void; hold(route: string): void; rejectHeld(): void; attempts(): number } } }

const hosts = ['settings', 'custom', 'grid', 'presets', 'personas', 'mobile'] as const
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
            await expect(failure.locator('span')).toHaveText(host === 'settings' || host === 'mobile' ? '설정 화면을 불러오지 못했습니다.' : '화면을 불러오지 못했습니다.')
            await expect(failure.getByRole('button')).toHaveText(['다시 시도', host === 'mobile' ? '뒤로' : '닫기'])
            await expect(failure.locator('p')).toHaveText('계속 실패할 경우 앱을 다시 실행해주세요.')
            await expect(failure.getByRole('button', { name: '취소', exact: true })).toHaveCount(0)
            if (action === 'retry') await failure.getByRole('button', { name: '다시 시도', exact: true }).click()
            else {
                await failure.getByRole('button', { name: host === 'mobile' ? '뒤로' : '닫기', exact: true }).click()
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
    await page.getByRole('button', { name: '캐릭터', exact: true }).click()
    await expect(page.getByRole('alert')).toHaveCount(0)
    await page.getByRole('button', { name: '설정', exact: true }).click()
    await expect(page.locator('[data-lazy-loaded]')).toHaveCount(1)
    expect(await page.evaluate(() => window.lazyImportBoundary.attempts())).toBe(2)
})

test('actual MobileBody ignores a prior import rejection after footer leave and re-entry', async ({ page }) => {
    const errors: string[] = []
    page.on('pageerror', error => errors.push(error.message))
    await page.goto('/lazyApp.html')
    await page.evaluate(() => { window.lazyImportBoundary.hold('mobile'); window.lazyApp.open('mobile') })
    await expect(page.getByRole('status')).toHaveText('로딩중')
    await page.getByRole('button', { name: '캐릭터', exact: true }).click()
    await expect(page.getByRole('status')).toHaveCount(0)
    await page.getByRole('button', { name: '설정', exact: true }).click()
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
    expect(await page.evaluate(() => window.lazyApp.read())).toEqual({ sidebarClosing: false, dialog: 'none', entries: 1 })
    await page.evaluate(() => history.back())
    await expect.poll(() => page.evaluate(() => window.lazyApp.read().sidebarClosing)).toBe(true)
    expect(page.url()).toMatch(/\/lazyApp\.html$/)
})

test('actual App cancels a dialog shown outside every layer on the Android root Back', async ({ page }) => {
    await page.goto('/lazyApp.html')
    const rootBack = () => page.evaluate(() => {
        const event = new Event('risunest-root-back', { cancelable: true })
        window.dispatchEvent(event)
        return event.defaultPrevented
    })
    expect(await rootBack()).toBe(false)
    await page.evaluate(() => window.lazyApp.showDialog())
    expect(await rootBack()).toBe(true)
    expect(await page.evaluate(() => window.lazyApp.read().dialog)).toBe('none')
    expect(await rootBack()).toBe(false)
})
