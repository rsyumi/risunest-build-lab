import { test, expect } from './fixture'

// The page itself never scrolls in the app, so a visually hidden checkbox that is positioned
// against the page instead of its control stays at its unscrolled place in a long list,
// stretches the page to it, and focusing it scrolls the whole app out of view. Chromium
// focuses the checkbox on click; WebKit only on keyboard focus, which the explicit focus stands for.
for (const kind of ['switch', 'setting']) {
    test(`the last ${kind} checkbox scrolls with its control and keeps the page in place`, async ({ page }) => {
        await page.goto(`/switchContainment.html?kind=${kind}`)
        const measure = () => page.evaluate(() => ({ scrollHeight: document.documentElement.scrollHeight, clientHeight: document.documentElement.clientHeight, scrollY }))
        const before = await measure()
        expect(before.scrollHeight).toBe(before.clientHeight)

        const list = page.locator('section')
        const last = list.locator('label:has(> input[type="checkbox"])').last()
        const input = last.locator('input')
        const offset = () => input.evaluate(element => element.getBoundingClientRect().top - element.parentElement!.getBoundingClientRect().top)
        const resting = await offset()
        await list.evaluate(element => { element.scrollTop = element.scrollHeight })
        expect(await list.evaluate(element => element.scrollTop)).toBeGreaterThan(0)
        expect(await offset()).toBe(resting)

        await last.click()
        await expect(input).toBeChecked()
        expect(await measure()).toEqual(before)

        await input.focus()
        await expect(input).toBeFocused()
        expect(await measure()).toEqual(before)
    })
}
