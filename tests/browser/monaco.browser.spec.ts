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
    // Monaco picks macOS key bindings from the user agent, which Playwright's WebKit reports on every host.
    const mac = await page.evaluate(() => navigator.userAgent.includes('Macintosh'))
    const keys = mac
        ? { end: 'Meta+ArrowDown', undo: 'Meta+z', redo: 'Meta+Shift+z', find: 'Meta+f', save: 'Meta+Enter' }
        : { end: 'Control+End', undo: 'Control+z', redo: 'Control+y', find: 'Control+f', save: 'Control+Enter' }
    for (const language of ['markdown', 'lua'] as const) {
        await page.evaluate(language => window.monacoDriver.open(language), language)
        const input = page.locator('#editor textarea.inputarea')
        await expect(input).toBeFocused()
        expect(await page.evaluate(() => window.monacoDriver.state())).toMatchObject({ language, editContext: false, languages: ['lua', 'markdown', 'plaintext'] })
        const original = await page.evaluate(() => window.monacoDriver.state().value)
        await page.keyboard.press(keys.end)
        await page.keyboard.type(' edited')
        await expect.poll(() => page.evaluate(() => window.monacoDriver.state().value)).toBe(`${original} edited`)
        await page.keyboard.press(keys.undo)
        await expect.poll(() => page.evaluate(() => window.monacoDriver.state().value)).toBe(original)
        await page.keyboard.press(keys.redo)
        await expect.poll(() => page.evaluate(() => window.monacoDriver.state().value)).toBe(`${original} edited`)
        await page.keyboard.press(keys.find)
        const find = page.locator('#editor').getByRole('textbox', { name: 'Find', exact: true })
        await expect(find).toBeFocused()
        await find.fill('hello')
        await expect(page.locator('#editor .find-widget .matchesCount')).toContainText('1 of 1')
        await page.keyboard.press('Escape')
        await expect(input).toBeFocused()
        await page.keyboard.press(keys.save)
        await expect.poll(() => page.evaluate(() => window.monacoDriver.state().saved)).toEqual([`${original} edited`])
    }
    expect(await page.evaluate(() => window.monacoDriver.diff())).toEqual([{ original: 2, modified: 2 }])
    const evidence = await page.evaluate(() => (window as unknown as { monacoWorkerEvidence: { urls: string[]; replies: number } }).monacoWorkerEvidence)
    expect(evidence.urls.length).toBeGreaterThan(0)
    expect(evidence.urls.every(url => /\/editor\.worker[^/]*\.js/.test(url))).toBe(true)
    expect(evidence.replies).toBeGreaterThan(0)
    expect(errors).toEqual([])
})

test('an IME composition covers its line in the line font and hides while the line is scrolled out', async ({ page, browserName }) => {
    test.skip(browserName !== 'chromium', 'Composes through the Chromium DevTools protocol')
    await page.goto('/monaco.html')
    await page.waitForFunction(() => !!window.monacoDriver)
    await page.evaluate(() => window.monacoDriver.open('markdown'))
    await expect(page.locator('#editor textarea.inputarea')).toBeFocused()
    const lines = Array.from({ length: 200 }, (_, index) => index === 11 ? '안녕하세요.' : `line ${index + 1}`)
    await page.evaluate(text => window.monacoDriver.place(text, 12, 7), lines.join('\n'))
    const cdp = await page.context().newCDPSession(page)
    const compose = async (...steps: string[]) => {
        for (const step of steps) await cdp.send('Input.imeSetComposition', { text: step, selectionStart: step.length, selectionEnd: step.length })
    }
    const composition = (line: number) => page.evaluate(line => window.monacoDriver.composition(line), line)

    await compose('ㅇ', '아', '안')
    await expect.poll(() => composition(12)).toEqual({ composing: true, background: 'rgb(33, 34, 44)', fontMatchesLine: true, visible: true, onLine: true })
    await page.evaluate(() => window.monacoDriver.scrollBy(600))
    await expect.poll(() => composition(12)).toMatchObject({ composing: true, visible: false })
    await page.evaluate(() => window.monacoDriver.scrollBy(-600))
    await expect.poll(() => composition(12)).toMatchObject({ composing: true, visible: true, onLine: true })
    await cdp.send('Input.insertText', { text: '안' })
    await expect.poll(() => page.evaluate(() => window.monacoDriver.state().value?.split('\n')[11])).toBe('안녕하세요.안')

    await page.evaluate(() => window.monacoDriver.scrollBy(600))
    const line = await page.evaluate(() => window.monacoDriver.firstVisibleLine() + 5)
    await page.evaluate(line => window.monacoDriver.moveTo(line, 1), line)
    await compose('ㄱ', '가')
    await expect.poll(() => composition(line)).toMatchObject({ composing: true, visible: true, onLine: true })
})
