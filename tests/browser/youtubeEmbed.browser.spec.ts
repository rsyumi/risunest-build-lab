import { readFileSync } from 'node:fs'
import type { Page } from '@playwright/test'
import { test, expect } from './fixture'
import { YOUTUBE_EMBED_PAGE_URL } from '../../src/ts/parser/youtubeEmbed'

const html = readFileSync(new URL('../../public/embed/youtube/v1.html', import.meta.url), 'utf8')
const wrapperOrigin = new URL(YOUTUBE_EMBED_PAGE_URL).origin
const wrap = (source: string) => `${YOUTUBE_EMBED_PAGE_URL}#${new URLSearchParams({ src: source })}`

async function mount(page: Page, source: string) {
    const requests: { url: string; referrer: string | undefined }[] = []
    const unexpected: string[] = []
    await page.route('**/*', async route => {
        const request = route.request()
        const url = new URL(request.url())
        if (url.hostname === '127.0.0.1' && url.pathname === '/youtube-test-host') {
            await route.fulfill({ contentType: 'text/html', body: '<!doctype html><title>Synthetic host</title><body></body>' })
        } else if (`${url.origin}${url.pathname}${url.search}` === YOUTUBE_EMBED_PAGE_URL) {
            await route.fulfill({ contentType: 'text/html', body: html })
        } else if (['https://www.youtube.com', 'https://www.youtube-nocookie.com'].includes(url.origin) && url.pathname.startsWith('/embed/')) {
            requests.push({ url: request.url(), referrer: request.headers()['referer'] })
            await route.fulfill({ contentType: 'text/html', body: '<!doctype html><title>Synthetic player</title><body>Player</body>' })
        } else {
            unexpected.push(request.url())
            await route.abort()
        }
    })
    await page.goto('/youtube-test-host')
    await page.evaluate(({ src, origin }) => {
        const frame = document.createElement('iframe')
        frame.id = 'player'
        frame.width = '560'
        frame.height = '315'
        frame.allow = `autoplay ${origin}; encrypted-media ${origin}; fullscreen ${origin}; picture-in-picture ${origin}`
        frame.allowFullscreen = true
        frame.src = src
        document.body.append(frame)
    }, { src: wrap(source), origin: wrapperOrigin })
    return { requests, unexpected, wrapper: page.frameLocator('#player') }
}

for (const host of ['www.youtube.com', 'www.youtube-nocookie.com']) {
    test(`${host} receives an origin referrer with its playback options and frame size`, async ({ page }) => {
        const source = `https://${host}/embed/videoseries?list=PLsynthetic&start=30&end=60&autoplay=0&loop=1&controls=0&playsinline=1&cc_lang_pref=ko`
        const { requests, unexpected, wrapper } = await mount(page, source)
        await expect(wrapper.frameLocator('iframe').locator('body')).toHaveText('Player')
        expect(requests).toEqual([{ url: source, referrer: `${wrapperOrigin}/` }])
        expect(await wrapper.locator('iframe').evaluate(frame => ({ width: frame.clientWidth, height: frame.clientHeight }))).toEqual({ width: 560, height: 315 })
        const player = page.frames().find(frame => frame.url().startsWith(`https://${host}/embed/`))!
        expect(await player.evaluate(() => document.referrer)).toBe(`${wrapperOrigin}/`)
        expect(await player.evaluate(() => document.fullscreenEnabled)).toBe(true)
        expect(unexpected).toEqual([])
    })
}

test('uses the actual wrapper origin for Player API parameters without enabling it by default', async ({ page }) => {
    const source = 'https://www.youtube.com/embed/M7lc1UVf-VE?enablejsapi=1&origin=tauri%3A%2F%2Flocalhost&start=42'
    const { requests, unexpected, wrapper } = await mount(page, source)
    await expect(wrapper.frameLocator('iframe').locator('body')).toHaveText('Player')
    const result = new URL(requests[0].url)
    expect(result.searchParams.get('origin')).toBe(wrapperOrigin)
    expect(result.searchParams.get('enablejsapi')).toBe('1')
    expect(result.searchParams.get('start')).toBe('42')
    expect(unexpected).toEqual([])
})

test('updates the existing frame on fragment navigation and stops playback for invalid input', async ({ page }) => {
    const { requests, unexpected, wrapper } = await mount(page, 'https://www.youtube.com/embed/first?autoplay=0')
    await expect(wrapper.frameLocator('iframe').locator('body')).toHaveText('Player')
    const next = 'https://www.youtube-nocookie.com/embed/second?start=12'
    await page.locator('#player').evaluate((frame, src) => frame.setAttribute('src', src), wrap(next))
    await expect(wrapper.locator('iframe')).toHaveAttribute('src', next)
    await expect(wrapper.frameLocator('iframe').locator('body')).toHaveText('Player')
    expect(requests.at(-1)?.url).toBe(next)
    await page.locator('#player').evaluate((frame, src) => frame.setAttribute('src', src), wrap('https://evil.example/embed/video'))
    await expect(wrapper.locator('#error')).toBeVisible()
    await expect(wrapper.locator('iframe')).toHaveCount(0)
    await page.locator('#player').evaluate((frame, src) => frame.setAttribute('src', src), wrap(next))
    await expect(wrapper.frameLocator('iframe').locator('body')).toHaveText('Player')
    await expect(wrapper.locator('#error')).toBeHidden()
    await expect(wrapper.locator('iframe')).toHaveCount(1)
    expect(unexpected).toEqual([])
})

test('rejects malformed and foreign destinations before creating any child frame', async ({ page }) => {
    const { requests, unexpected, wrapper } = await mount(page, 'http://www.youtube.com/embed/video')
    await expect(wrapper.locator('#error')).toBeVisible()
    const frame = page.frames().find(frame => frame.url().startsWith(YOUTUBE_EMBED_PAGE_URL))!
    for (const hash of [
        '', '#src=', '#other=value',
        '#src=https://www.youtube.com/embed/first&src=https://www.youtube.com/embed/second',
        ...[
            'http://www.youtube.com/embed/video', 'https://www.youtube.com.evil.example/embed/video',
            'https://user@www.youtube.com/embed/video', 'https://www.youtube.com:444/embed/video',
            'https://www.youtube.com/embed/../watch?v=video', 'https://www.youtube-nocookie.com/watch?v=video',
        ].map(src => `#${new URLSearchParams({ src })}`),
    ]) {
        // Reload each invalid input so this also covers direct visits to the public page.
        await frame.goto('about:blank')
        await frame.goto(`${YOUTUBE_EMBED_PAGE_URL}${hash}`)
        await expect(wrapper.locator('#error')).toBeVisible()
        await expect(wrapper.locator('iframe')).toHaveCount(0)
    }
    expect(requests).toEqual([])
    expect(unexpected).toEqual([])
})
