// @vitest-environment jsdom

import { describe, it, expect, vi } from 'vitest'
import { writable } from 'svelte/store'
import { ParseMarkdown, parseOnboardingHTML, trimMarkdown } from '../parser.svelte'
import { DeferredInlayMarkerRegistry } from '../../process/files/inlayRenderSource'

// On desktop getFileSrc resolves a local asset to an asset: URL. The sanitizer must keep such
// a src, or every {{img::}}, {{asset::}} and {{emotion::}} asset renders as its broken path.
const assetUrl =
  'asset://localhost/Users/test/Library/Application%20Support/co.aiclient.risu/assets/pic.png'

vi.mock('../../platform', async () => (await import('../../platform.testSupport')).platformModule({ runtime: 'web', os: 'windows' }))

//#region module mocks

vi.mock(
  import('../../storage/database.svelte'),
  () =>
    ({
      appVer: '1234.5.67',
      getCurrentCharacter: () => ({}),
      getCurrentChat: () => ({}),
      getDatabase: () => ({}),
    } as typeof import('../../storage/database.svelte'))
)

vi.mock(import('../../globalApi.svelte'), () => ({
  aiWatermarkingLawApplies: () => false,
  getFileSrc: () => Promise.resolve(assetUrl),
}))

vi.mock(import('../../stores.svelte'), () => {
  return {
    DBState: {
      db: {
        characters: [
          {
            chatPage: 0,
            chats: [{}],
            defaultVariables: '',
          },
        ],
        globalChatVariables: {},
        templateDefaultVariables: '',
      },
    },
    selIdState: {
      selId: 0,
    },
    selectedCharID: writable(0),
  } as typeof import('../../stores.svelte')
})

//#endregion

const imageSrc = (html: string) => {
  const root = document.createElement('div')
  root.innerHTML = html
  return root.querySelector('img')?.getAttribute('src')
}

describe('asset: image src sanitization', () => {
  it('keeps the src of an img whose URL uses the asset: scheme', () => {
    expect(imageSrc(trimMarkdown(`<img src="${assetUrl}" alt="${assetUrl}">`))).toBe(assetUrl)
  })

  it('keeps the asset: src through the ParseMarkdown chat entry point', async () => {
    expect(imageSrc(await ParseMarkdown(`<img src="${assetUrl}" alt="${assetUrl}">`, null, 'back'))).toBe(assetUrl)
  })

  it('keeps a resolved character asset src in onboarding HTML', async () => {
    const char = {
      type: 'character', chaId: 'synthetic', name: 'Synthetic',
      additionalAssets: [['pic', 'assets/pic.png', 'png']], emotionImages: [],
    } as any
    const registry = new DeferredInlayMarkerRegistry()
    try {
      expect(imageSrc(await parseOnboardingHTML('{{img::pic}}', char, registry))).toBe(assetUrl)
    } finally {
      registry.clear()
    }
  })
})

describe('YouTube iframe sanitization', () => {
  const pageUrl = 'https://update.rsyumi.workers.dev/embed/youtube/v1'
  const frame = (html: string) => {
    const root = document.createElement('div')
    root.innerHTML = html
    return root.querySelector('iframe')
  }

  it.each(['www.youtube.com', 'www.youtube-nocookie.com'])('renders %s through the shared page without losing playback options', async host => {
    const source = `https://${host}/embed/M7lc1UVf-VE?start=30&end=60&loop=1&playlist=M7lc1UVf-VE&controls=0&playsinline=1`
    const input = `<iframe src="${source.replaceAll('&', '&amp;')}" width="560" height="315" style="border: 0" allow="autoplay https://${host}; encrypted-media 'none'; picture-in-picture" allowfullscreen></iframe>`
    const output = frame(await ParseMarkdown(input, null, 'back'))!
    const url = new URL(output.src)
    expect(`${url.origin}${url.pathname}`).toBe(pageUrl)
    expect(url.search).toBe('')
    expect(new URLSearchParams(url.hash.slice(1)).get('src')).toBe(source)
    expect(output.getAttribute('width')).toBe('560')
    expect(output.getAttribute('height')).toBe('315')
    expect(output.style.border).toBe('0px')
    expect(output.hasAttribute('allowfullscreen')).toBe(true)
    expect(output.getAttribute('allow')).toBe("autoplay https://update.rsyumi.workers.dev; encrypted-media 'none'; picture-in-picture")
  })

  it('keeps an already adapted iframe stable and preserves playlist embeds', () => {
    const source = 'https://www.youtube.com/embed/videoseries?list=PLsynthetic&autoplay=0'
    const first = trimMarkdown(`<iframe src="${source.replaceAll('&', '&amp;')}" allow="autoplay 'none'; fullscreen"></iframe>`)
    const second = trimMarkdown(first)
    expect(frame(first)!.src).toBe(`${pageUrl}#${new URLSearchParams({ src: source })}`)
    expect(frame(second)!.src).toBe(frame(first)!.src)
    expect(frame(second)!.getAttribute('allow')).toBe("autoplay 'none'; fullscreen")
  })

  it('uses the same adapter for character onboarding HTML', async () => {
    const char = {
      type: 'character', chaId: 'synthetic', name: 'Synthetic', additionalAssets: [], emotionImages: [],
    } as any
    const registry = new DeferredInlayMarkerRegistry()
    const source = 'https://www.youtube-nocookie.com/embed/M7lc1UVf-VE'
    try {
      const output = await parseOnboardingHTML(`<iframe src="${source}" allow="autoplay *; fullscreen 'src'"></iframe>`, char, registry)
      expect(frame(output)!.src).toBe(`${pageUrl}#${new URLSearchParams({ src: source })}`)
      expect(frame(output)!.getAttribute('allow')).toBe("autoplay *; fullscreen 'src'")
    } finally {
      registry.clear()
    }
  })

  it.each([
    'https://www.youtube.com.evil.example/embed/M7lc1UVf-VE',
    'https://www.youtube.com@evil.example/embed/M7lc1UVf-VE',
    'https://user@www.youtube.com/embed/M7lc1UVf-VE',
    'https://www.youtube.com:444/embed/M7lc1UVf-VE',
    'http://www.youtube.com/embed/M7lc1UVf-VE',
    'https://www.youtube.com/watch?v=M7lc1UVf-VE',
    'https://www.youtube.com/embed/../watch?v=M7lc1UVf-VE',
    `${pageUrl}#src=${encodeURIComponent('https://evil.example/embed/video')}`,
    `${pageUrl}?src=https://www.youtube.com/embed/M7lc1UVf-VE`,
    `${pageUrl}#src=https://www.youtube.com/embed/M7lc1UVf-VE&src=https://www.youtube.com/embed/other`,
    'https://update.rsyumi.workers.dev/oauth/google-drive-callback',
  ])('removes unsupported iframe destinations: %s', source => {
    expect(frame(trimMarkdown(`<iframe src="${source.replaceAll('&', '&amp;')}"></iframe>`))).toBeNull()
  })
})
