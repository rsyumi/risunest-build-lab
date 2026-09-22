import { test, expect } from './fixture'

test('real iframe clones payloads and delivers responses asynchronously', async ({ page }) => {
    await page.goto('/')
    await page.evaluate(() => window.boundary.load(`
        const payload = { text: 'sent' };
        const order = [];
        const pending = risuai.pluginStorage.setItem('payload', payload);
        payload.text = 'mutated'; order.push('same-turn');
        await pending; order.push('response');
        await risuai.pluginStorage.setItem('order', order);
    `))
    await expect.poll(() => page.evaluate(() => window.boundary.state.writes)).toEqual([
        { key: 'payload', value: { text: 'sent' } }, { key: 'order', value: ['same-turn', 'response'] },
    ])
    await expect(page.locator('[data-risu-plugin-frame]')).toHaveAttribute('sandbox', 'allow-scripts allow-modals allow-downloads')
})

const callbackScript = `
try {
    window.addEventListener('message', event => {
        if (event.data.type === 'INVOKE_CALLBACK') parent.postMessage({fixture:'callback', reqId:event.data.reqId}, '*');
    });
    await risuai.addTTSPreprocessor(() => new Promise(resolve => {
        const release = event => {
            if (!event.data.fixtureRelease) return;
            window.removeEventListener('message', release); resolve('real');
        };
        window.addEventListener('message', release);
    }));
} catch (error) { parent.postMessage({fixture:'error', error:String(error)}, '*'); }
`

test('wrong-source reply is ignored and the real callback still resolves', async ({ page }) => {
    await page.goto('/')
    await page.evaluate(script => window.boundary.load(script), callbackScript)
    await expect.poll(() => page.evaluate(() => ({ size: window.boundary.state.callbacks.size, events: window.boundary.events }))).toMatchObject({ size: 1 })
    await page.evaluate(() => window.boundary.call())
    await expect.poll(() => page.evaluate(() => window.boundary.events.length)).toBe(1)
    await page.evaluate(() => window.boundary.forge((window.boundary.events[0] as { reqId: string }).reqId))
    await expect.poll(() => page.evaluate(() => window.boundary.events.length)).toBe(2)
    expect(await page.evaluate(() => window.boundary.callbackResult)).toBeNull()
    await page.evaluate(() => window.boundary.release())
    await expect.poll(() => page.evaluate(() => window.boundary.callbackResult)).toEqual({ value: 'real' })
})

test('unload rejects an owned pending callback and the next frame works', async ({ page }) => {
    await page.goto('/')
    await page.evaluate(script => window.boundary.load(script), callbackScript)
    await expect.poll(() => page.evaluate(() => ({ size: window.boundary.state.callbacks.size, events: window.boundary.events }))).toMatchObject({ size: 1 })
    await page.evaluate(() => window.boundary.call())
    await expect.poll(() => page.evaluate(() => window.boundary.events.length)).toBe(1)
    const oldId = await page.evaluate(() => (window.boundary.events[0] as { reqId: string }).reqId)
    await page.evaluate(() => window.boundary.unload())
    await expect.poll(() => page.evaluate(() => window.boundary.callbackResult)).toEqual({ error: 'Error: Plugin sandbox terminated' })
    expect(await page.evaluate(() => window.boundary.released)).toBe(true)
    await page.evaluate(script => window.boundary.load(script), callbackScript)
    await expect.poll(() => page.evaluate(() => ({ size: window.boundary.state.callbacks.size, events: window.boundary.events }))).toMatchObject({ size: 1 })
    await page.evaluate(() => window.boundary.call())
    await page.evaluate(id => window.boundary.forge(id), oldId)
    await expect.poll(() => page.evaluate(() => window.boundary.events.length)).toBe(3)
    expect(await page.evaluate(() => window.boundary.callbackResult)).toBeNull()
    await page.evaluate(() => window.boundary.release())
    await expect.poll(() => page.evaluate(() => window.boundary.callbackResult)).toEqual({ value: 'real' })
})

test('a late host response cannot resume an unloaded plugin or affect its replacement', async ({ page }) => {
    await page.goto('/')
    await page.evaluate(() => window.boundary.load(`
        const value = await risuai.pluginStorage.getItem('held');
        await risuai.pluginStorage.setItem('old-result', value);
    `))
    await expect.poll(() => page.evaluate(() => !!window.boundary.state.releaseHeld)).toBe(true)
    await page.evaluate(() => window.boundary.unload())
    await page.evaluate(() => window.boundary.load(`
        const value = await risuai.pluginStorage.getItem('current');
        await risuai.pluginStorage.setItem('new-result', value);
    `))
    await expect.poll(() => page.evaluate(() => window.boundary.state.writes)).toEqual([{ key: 'new-result', value: 'current-value' }])
    await page.evaluate(() => window.boundary.releaseHeld())
    await expect.poll(() => page.evaluate(() => window.boundary.oldCallsFinished)).toBe(true)
    expect(await page.evaluate(() => window.boundary.state.writes)).toEqual([{ key: 'new-result', value: 'current-value' }])
})
