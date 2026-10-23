import { readFile } from 'node:fs/promises'
import { afterEach, beforeAll, beforeEach, describe, expect, it, vi } from 'vitest'

const text = '<|turn>user\nHello<turn|>'
const expectedIds = [105, 2364, 107, 9259, 106]
let model: Uint8Array<ArrayBuffer>
let assetReads: number
let fetchModel: () => Promise<Response>

beforeAll(async () => {
    model = new Uint8Array(await readFile('public/token/gemma/gemma4.json'))
})

beforeEach(() => {
    vi.resetModules()
    assetReads = 0
    fetchModel = async () => new Response(model)
    vi.stubGlobal('fetch', async (url: string) => {
        if (url !== '/token/gemma/gemma4.json') {
            throw new Error(`External or unexpected asset request: ${url}`)
        }
        assetReads++
        return fetchModel()
    })
})

afterEach(() => {
    vi.unstubAllGlobals()
})

describe('bundled Gemma 4 tokenizer', () => {
    it('loads on demand and shares initialization', async () => {
        const { encodeGemma } = await import('./gemmaTokenizer')
        expect(assetReads).toBe(0)
        const outputs = await Promise.all([encodeGemma(text), encodeGemma('Hello, world!'), encodeGemma(text)])
        expect(outputs.map(ids => [...ids])).toEqual([
            expectedIds, [9259, 236764, 1902, 236888], expectedIds,
        ])
        expect([...await encodeGemma(text)]).toEqual(expectedIds)
        expect(assetReads).toBe(1)
    })

    it.each(['read', 'http', 'parse'] as const)('allows retry after an asset %s failure', async failure => {
        fetchModel = async () => {
            if (failure === 'read') throw new Error('Asset read failed')
            return failure === 'http'
                ? new Response('', { status: 503 })
                : new Response('invalid tokenizer JSON')
        }
        const { encodeGemma } = await import('./gemmaTokenizer')
        await expect(encodeGemma(text)).rejects.toThrow(
            failure === 'read' ? 'Asset read failed' : failure === 'http' ? 'HTTP 503' : undefined,
        )
        fetchModel = async () => new Response(model)
        expect([...await encodeGemma(text)]).toEqual(expectedIds)
        expect(assetReads).toBe(2)
    })
})
