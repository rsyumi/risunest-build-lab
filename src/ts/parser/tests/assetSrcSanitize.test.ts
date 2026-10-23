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
