import { afterEach, describe, expect, test, vi } from 'vitest'

// The app graph runs store effects on import that a loaded database would feed.
vi.mock('src/ts/parser/parser.svelte', async () => (await import('src/ts/parser/parser.testSupport')).parserStubModule())

import 'src/ts/stores.svelte'
import * as globalApi from 'src/ts/globalApi.svelte'
import { setDatabaseLite, type character } from 'src/ts/storage/database.svelte'
import { generateAIImage, type ImageOrientation } from './stableDiff'

afterEach(() => vi.restoreAllMocks())

describe('NovelAI image orientation', () => {
    test.each([
        { width: 1024, height: 640, orientation: undefined, sent: { width: 1024, height: 640 } },
        { width: 640, height: 1024, orientation: 'landscape', sent: { width: 1024, height: 640 } },
        { width: 1024, height: 640, orientation: 'portrait', sent: { width: 640, height: 1024 } },
    ] satisfies { width: number, height: number, orientation?: ImageOrientation, sent: object }[])(
        'sends a $width x $height configuration as $sent.width x $sent.height for $orientation',
        async ({ width, height, orientation, sent }) => {
            setDatabaseLite({
                sdProvider: 'novelai',
                NAIImgModel: 'nai-diffusion-4-5-full',
                NAIImgUrl: 'https://image.example.invalid/generate',
                NAIApiKey: 'synthetic-key',
                NAII2I: false,
                NAIImgConfig: { width, height },
            } as never)
            const fetch = vi.spyOn(globalApi, 'globalFetch').mockResolvedValue({
                ok: false, data: Buffer.from('synthetic failure'), headers: {}, status: 500,
            })
            vi.spyOn(console, 'log').mockImplementation(() => undefined)
            vi.spyOn(console, 'error').mockImplementation(() => undefined)

            await generateAIImage('prompt', { image: '' } as character, '', 'inlay', { orientation })

            expect(fetch.mock.calls.map(([, request]) => ({
                width: request.body.parameters.width,
                height: request.body.parameters.height,
            }))).toEqual([sent])
        },
    )
})
