import { test, expect } from './fixture'
import type {} from './monacoMain'

test('real markdown and Lua editors retain editing, find, undo and save with a responding editor worker', async ({ page }) => {
    const errors: string[] = []
    page.on('pageerror', error => errors.push(error.message))
    page.on('console', message => {
        if (/Could not create web worker|falling back to loading web worker/i.test(message.text())) errors.push(message.text())
    })
    await page.addInitScript(() => {
        const NativeWorker = window.Worker
        const evidence = { urls: [] as string[], replies: 0 }
        Object.assign(window, { monacoWorkerEvidence: evidence })
        window.Worker = class extends NativeWorker {
            constructor(url: string | URL, options?: WorkerOptions) {
                super(url, options)
                evidence.urls.push(String(url))
                this.addEventListener('message', () => { evidence.replies++ })
            }
        }
    })
    await page.goto('/monaco.html')
    await page.waitForFunction(() => !!window.monacoDriver)
    for (const language of ['markdown', 'lua'] as const) {
        await page.evaluate(language => window.monacoDriver.open(language), language)
        const input = page.locator('#editor textarea.inputarea')
        await expect(input).toBeFocused()
        expect(await page.evaluate(() => window.monacoDriver.state())).toMatchObject({ language, editContext: false, languages: ['lua', 'markdown', 'plaintext'] })
        const original = await page.evaluate(() => window.monacoDriver.state().value)
        await page.keyboard.press('Control+End')
        await page.keyboard.type(' edited')
        await expect.poll(() => page.evaluate(() => window.monacoDriver.state().value)).toBe(`${original} edited`)
        await page.keyboard.press('Control+z')
        await expect.poll(() => page.evaluate(() => window.monacoDriver.state().value)).toBe(original)
        await page.keyboard.press('Control+y')
        await expect.poll(() => page.evaluate(() => window.monacoDriver.state().value)).toBe(`${original} edited`)
        await page.keyboard.press('Control+f')
        const find = page.locator('#editor .find-widget .monaco-inputbox input').first()
        await expect(find).toBeFocused()
        await find.fill('hello')
        await expect(page.locator('#editor .find-widget .matchesCount')).toContainText('1 of 1')
        await page.keyboard.press('Escape')
        await expect(input).toBeFocused()
        await page.keyboard.press('Control+Enter')
        await expect.poll(() => page.evaluate(() => window.monacoDriver.state().saved)).toEqual([`${original} edited`])
    }
    expect(await page.evaluate(() => window.monacoDriver.diff())).toEqual([{ original: 2, modified: 2 }])
    const evidence = await page.evaluate(() => (window as unknown as { monacoWorkerEvidence: { urls: string[]; replies: number } }).monacoWorkerEvidence)
    expect(evidence.urls.length).toBeGreaterThan(0)
    expect(evidence.urls.every(url => /\/editor\.worker[^/]*\.js/.test(url))).toBe(true)
    expect(evidence.replies).toBeGreaterThan(0)
    expect(errors).toEqual([])
})
