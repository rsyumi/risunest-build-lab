import { expect, it, vi } from 'vitest'

const translateModel = vi.hoisted(() => vi.fn())
vi.mock('@browsermt/bergamot-translator', () => ({
    TranslatorBacking: class {
        constructor(_options?: unknown) {}
        async loadModelRegistery(): Promise<unknown[]> { return [] }
    },
    LatencyOptimisedTranslator: class {
        translate(request: { text: string }) { return translateModel(request) }
    },
}))

// The model cache never runs here; the stub keeps util's import graph out.
vi.mock('../util', () => ({ asBuffer: <T,>(value: T) => value }) satisfies Partial<Record<keyof typeof import('../util'), unknown>>)

import { bergamotTranslate } from './bergamotTranslator'

it('keeps translating after one translation fails', async () => {
    translateModel
        .mockRejectedValueOnce(new Error('synthetic model failure'))
        .mockImplementation(async ({ text }: { text: string }) => ({ target: { text: `translated ${text}` } }))

    await expect(bergamotTranslate('first', 'en', 'ko', false)).rejects.toThrow('synthetic model failure')
    await expect(bergamotTranslate('second', 'en', 'ko', false)).resolves.toBe('translated second')
})
