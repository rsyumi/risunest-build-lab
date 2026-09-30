import { test, expect } from './fixture'

// A 500,049-unit repeated Korean/emoji thought took about 59 s of layout in
// one inline paragraph in desktop Chromium. The bounds below only catch a
// return of that stall; they are not a frame-rate benchmark.
const completionBoundMs = 20_000
const frameGapBoundMs = 2_000

test.use({ viewport: { width: 393, height: 760 }, deviceScaleFactor: 2.75, isMobile: true, hasTouch: true })

for (const kind of ['repeated', 'ascii'] as const) {
    test(`expanding a large ${kind} thought reaches the exact end without a long stall`, async ({ page }) => {
        await page.goto('/')
        const sizes = await page.evaluate((kind) => window.thought.mount(kind), kind)
        expect(sizes.sourceLength).toBeGreaterThanOrEqual(500_000)
        const started = await page.evaluate(() => window.thought.toggle())
        await page.waitForFunction(() => window.thought.state.complete, undefined, { timeout: completionBoundMs, polling: 100 })
        const finished = await page.evaluate(() => performance.now())
        const state = await page.evaluate(() => window.thought.state)
        test.info().annotations.push({ type: 'timing', description: JSON.stringify({ kind, completeMs: Math.round(finished - started), maxFrameGapMs: Math.round(state.maxFrameGap), units: state.units }) })
        expect(state.open).toBe(true)
        expect(state.length).toBe(sizes.expectedLength)
        expect(state.endsWithLatest).toBe(true)
        expect(state.units).toBeGreaterThan(200)
        expect(state.maxFrameGap).toBeLessThan(frameGapBoundMs)
    })
}

test('collapsing during the incremental expansion is handled promptly and stops the work', async ({ page }) => {
    await page.goto('/')
    await page.evaluate(() => window.thought.mount('repeated'))
    await page.evaluate(() => window.thought.toggle())
    await page.evaluate(() => window.thought.frames(2))
    const partial = await page.evaluate(() => window.thought.state)
    expect(partial.open).toBe(true)
    expect(partial.complete).toBe(false)
    const clicked = await page.evaluate(() => window.thought.toggle())
    await page.evaluate(() => window.thought.frames(10))
    const collapsed = await page.evaluate(() => window.thought.state)
    expect(collapsed.open).toBe(false)
    expect(collapsed.units).toBe(0)
    const closing = collapsed.toggles.find((toggle) => !toggle.open)
    expect(closing).toBeDefined()
    expect(closing!.at - clicked).toBeLessThan(frameGapBoundMs)
    expect(collapsed.maxFrameGap).toBeLessThan(frameGapBoundMs)
})
