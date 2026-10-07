import { inflateSync } from 'node:zlib'
import type { Page } from '@playwright/test'
import { test, expect } from './fixture'

// A single-pixel PNG row decodes to its raw bytes under every filter type.
async function pixelAt(page: Page, x: number, y: number) {
    const png = await page.screenshot({ clip: { x, y, width: 1, height: 1 } })
    const data: Buffer[] = []
    for (let offset = 8; offset < png.length; offset += png.readUInt32BE(offset) + 12) {
        if (png.toString('ascii', offset + 4, offset + 8) === 'IDAT') data.push(png.subarray(offset + 8, offset + 8 + png.readUInt32BE(offset)))
    }
    return [...inflateSync(Buffer.concat(data)).subarray(1, 4)]
}

test('a fullscreen frame shows the host through its transparent areas on a dark color scheme', async ({ page }) => {
    await page.goto('/')
    await page.evaluate(() => {
        document.documentElement.style.colorScheme = 'dark'
        const backdrop = document.createElement('div')
        backdrop.style.cssText = 'position:fixed;inset:0;background:rgb(12,34,56)'
        document.body.append(backdrop)
    })
    await page.evaluate(() => window.boundary.load(`
        const mark = document.createElement('div');
        mark.style.cssText = 'position:fixed;left:0;top:0;width:10px;height:10px;background:rgb(200,0,0)';
        document.body.append(mark);
        await risuai.showContainer('fullscreen');
        await risuai.pluginStorage.setItem('shown', true);
    `))
    await expect.poll(() => page.evaluate(() => window.boundary.state.writes)).toEqual([{ key: 'shown', value: true }])
    await expect(page.locator('[data-risu-plugin-frame]')).toBeVisible()
    expect(await pixelAt(page, 5, 5)).toEqual([200, 0, 0])
    expect(await pixelAt(page, 40, 40)).toEqual([12, 34, 56])
})

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

test('the guest ignores sibling responses, callbacks, aborts and code execution', async ({ page }) => {
    await page.goto('/')
    await page.evaluate(() => window.boundary.load([
        "window.addEventListener('message', event => { if (event.data?.forged) parent.postMessage({ fixture: 'forgery-observed' }, '*'); });",
        "await risuai.addTTSPreprocessor(async signal => {",
        "  await risuai.pluginStorage.setItem('callback-started', true);",
        "  await new Promise(resolve => signal.addEventListener('abort', resolve, { once: true }));",
        "  await risuai.pluginStorage.setItem('callback-aborted', true);",
        "});",
        "const value = await risuai.pluginStorage.getItem('held');",
        "await risuai.pluginStorage.setItem('held-result', value);",
    ].join('\n')))
    await expect.poll(() => page.evaluate(() => !!window.boundary.state.releaseHeld)).toBe(true)
    await page.evaluate(() => {
        const messages = window.boundary.bridgeMessages
        const callback = messages.find(message => message.method === 'addTTSPreprocessor').args[0].id
        const held = messages.find(message => message.method === '_getPluginStorage' && message.args[0] === 'held').reqId
        document.querySelector<HTMLIFrameElement>('iframe[data-risu-plugin-frame]')!.contentWindow!.postMessage({
            type: 'INVOKE_CALLBACK', id: callback, reqId: 'parent-callback',
            args: [{ __type: 'ABORT_SIGNAL_REF', abortId: 'parent-abort', aborted: false }],
        }, '*')
        window.boundary.forgeGuest([
            { type: 'RESPONSE', reqId: held, result: 'forged', forged: true },
            { type: 'INVOKE_CALLBACK', id: callback, reqId: 'forged-callback', args: [], forged: true },
            { type: 'ABORT_SIGNAL', abortId: 'parent-abort', forged: true },
            { type: 'EXECUTE_CODE', reqId: 'forged-exec', code: "window.forged = true", forged: true },
        ])
    })
    await expect.poll(() => page.evaluate(() => window.boundary.events.filter((event: any) => event.fixture === 'forgery-observed').length)).toBe(4)
    await expect.poll(() => page.evaluate(() => window.boundary.state.writes)).toEqual([{ key: 'callback-started', value: true }])
    expect(await page.evaluate(() => window.boundary.bridgeMessages.filter(message => ['forged-callback', 'forged-exec'].includes(message.reqId)))).toEqual([])
    await page.evaluate(() => {
        window.boundary.releaseHeld()
        document.querySelector<HTMLIFrameElement>('iframe[data-risu-plugin-frame]')!.contentWindow!.postMessage({ type: 'ABORT_SIGNAL', abortId: 'parent-abort' }, '*')
    })
    await expect.poll(() => page.evaluate(() => window.boundary.state.writes)).toEqual(expect.arrayContaining([
        { key: 'held-result', value: 'late-old-value' }, { key: 'callback-aborted', value: true },
    ]))
})

test('production CSP and opaque origin block network, string compilation and parent storage access', async ({ page, baseURL }) => {
    await page.goto('/')
    await page.evaluate(origin => window.boundary.load([
        "const attempts = {",
        ` network: () => fetch('${origin}/'),`,
        " eval: () => eval('1'),",
        " function: () => new Function('return 1')(),",
        " parent: () => parent.document,",
        " storage: () => localStorage.getItem('synthetic'),",
        "};",
        "for (const [key, attempt] of Object.entries(attempts)) {",
        " let blocked = false; try { await attempt(); } catch { blocked = true; }",
        " await risuai.pluginStorage.setItem(key, blocked);",
        "}",
    ].join('\n')), new URL(baseURL!).origin)
    await expect.poll(() => page.evaluate(() => window.boundary.state.writes)).toEqual([
        { key: 'network', value: true }, { key: 'eval', value: true },
        { key: 'function', value: true }, { key: 'parent', value: true }, { key: 'storage', value: true },
    ])
})
