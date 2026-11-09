import { describe, expect, it, vi } from 'vitest'
import {
    NATIVE_TOKENIZER_MAX_AGGREGATE_INPUT_BYTES,
    NATIVE_TOKENIZER_MAX_BATCH_ITEMS,
    resolveProductionNativeTokenizerId,
    tryNativeTokenizerIdsBatch,
    type NativeTokenizerIdsBatchCandidate,
    type ProductionNativeTokenizerContext,
} from './nativeTokenizerProduction'
import { NATIVE_MODEL_TOKENIZER_FINGERPRINTS, NATIVE_TOKENIZER_FINGERPRINTS } from './nativeTokenizer'

const cl100kContext: ProductionNativeTokenizerContext = {
    isTauri: true,
    aiModel: 'gpt-4',
    customTokenizer: 'tik',
    modelTokenizerId: 'cl100k_base',
}

function candidate(texts: string[]): NativeTokenizerIdsBatchCandidate {
    return {
        itemCount: texts.length,
        aggregateInputBytes: () =>
            texts.reduce((total, text) => total + new TextEncoder().encode(text).byteLength, 0),
        buildTexts: () => texts,
    }
}

describe('production native tokenizer eligibility', () => {
    it('keeps Web on the existing JavaScript route', async () => {
        const invoke = vi.fn()
        const webMeasure = vi.fn(() => 0)
        const webBuild = vi.fn(() => [] as string[])

        await expect(
            tryNativeTokenizerIdsBatch(
                {
                    itemCount: 3,
                    aggregateInputBytes: webMeasure,
                    buildTexts: webBuild,
                },
                { ...cl100kContext, isTauri: false },
                invoke,
            ),
        ).resolves.toBeNull()
        expect(invoke).not.toHaveBeenCalled()
        expect(webMeasure).not.toHaveBeenCalled()
        expect(webBuild).not.toHaveBeenCalled()
    })

    it('rejects unsupported and oversized candidates before building a batch or invoking IPC', async () => {
        const invoke = vi.fn()
        const unsupportedMeasure = vi.fn(() => 0)
        const unsupportedBuild = vi.fn(() => [] as string[])
        const tooManyMeasure = vi.fn(() => 0)
        const tooManyBuild = vi.fn(() => [] as string[])
        const oversizedMeasure = vi.fn(() => NATIVE_TOKENIZER_MAX_AGGREGATE_INPUT_BYTES + 1)
        const oversizedBuild = vi.fn(() => [] as string[])

        await expect(
            tryNativeTokenizerIdsBatch(
                {
                    itemCount: 3,
                    aggregateInputBytes: unsupportedMeasure,
                    buildTexts: unsupportedBuild,
                },
                { ...cl100kContext, modelTokenizerId: null },
                invoke,
            ),
        ).resolves.toBeNull()
        await expect(
            tryNativeTokenizerIdsBatch(
                {
                    itemCount: NATIVE_TOKENIZER_MAX_BATCH_ITEMS + 1,
                    aggregateInputBytes: tooManyMeasure,
                    buildTexts: tooManyBuild,
                },
                cl100kContext,
                invoke,
            ),
        ).resolves.toBeNull()
        await expect(
            tryNativeTokenizerIdsBatch(
                {
                    itemCount: 3,
                    aggregateInputBytes: oversizedMeasure,
                    buildTexts: oversizedBuild,
                },
                cl100kContext,
                invoke,
            ),
        ).resolves.toBeNull()

        expect(invoke).not.toHaveBeenCalled()
        expect(unsupportedMeasure).not.toHaveBeenCalled()
        expect(unsupportedBuild).not.toHaveBeenCalled()
        expect(tooManyMeasure).not.toHaveBeenCalled()
        expect(tooManyBuild).not.toHaveBeenCalled()
        expect(oversizedMeasure).toHaveBeenCalledTimes(1)
        expect(oversizedBuild).not.toHaveBeenCalled()
    })

    it.each([
        [{ ...cl100kContext, aiModel: 'openrouter', customTokenizer: 'tik' }, 'o200k_base'],
        [{ ...cl100kContext, aiModel: 'reverse_proxy', customTokenizer: 'tik' }, 'o200k_base'],
        [{ ...cl100kContext, aiModel: 'openrouter', customTokenizer: 'o200k_base' }, 'o200k_base'],
        [{ ...cl100kContext, aiModel: 'reverse_proxy', customTokenizer: 'toString' }, 'o200k_base'],
        [
            {
                ...cl100kContext,
                aiModel: 'custom',
                modelTokenizerId: null,
                pluginTokenizer: 'cl100k_base',
            },
            'cl100k_base',
        ],
        [
            {
                ...cl100kContext,
                aiModel: 'custom',
                modelTokenizerId: null,
                pluginTokenizer: 'o200k_base',
            },
            'o200k_base',
        ],
        [{ ...cl100kContext, aiModel: 'custom', modelTokenizerId: null }, 'o200k_base'],
        [{ ...cl100kContext, aiModel: 'custom', pluginTokenizer: '' }, 'cl100k_base'],
        [{ ...cl100kContext, aiModel: 'openrouter', customTokenizer: 'mistral' }, 'mistral'],
        [{ ...cl100kContext, aiModel: 'reverse_proxy', customTokenizer: 'deepseek-v4' }, 'deepseek-v4'],
        [{ ...cl100kContext, aiModel: 'custom', modelTokenizerId: null, pluginTokenizer: 'glm5' }, 'glm5'],
        [{ ...cl100kContext, modelTokenizerId: 'gemma' }, 'gemma'],
    ] as const)('resolves the tokenizer encode runs from %o', (context, expected) => {
        expect(resolveProductionNativeTokenizerId(context)).toBe(expected)
    })

    it.each([
        {
            ...cl100kContext,
            aiModel: 'custom',
            modelTokenizerId: null,
            pluginTokenizer: 'custom',
        },
        { ...cl100kContext, modelTokenizerId: null },
        { ...cl100kContext, modelTokenizerId: 'novellist' },
        { ...cl100kContext, aiModel: 'openrouter', customTokenizer: 'novellist' },
        { ...cl100kContext, aiModel: 'custom', pluginTokenizer: 'novellist' },
    ] as const)('keeps route %o in JavaScript', (context) => {
        expect(resolveProductionNativeTokenizerId(context)).toBeNull()
    })

    it('invokes one ordered IDs batch', async () => {
        const texts = ['first segment', '두 번째 🙂', 'third']
        const ids = texts.map((_, index) => [index, index + 1])
        const invoke = vi.fn(async () => ({
            mode: 'ids',
            artifact_fingerprint: NATIVE_TOKENIZER_FINGERPRINTS.cl100k_base,
            ids,
        }))

        await expect(tryNativeTokenizerIdsBatch(candidate(texts), cl100kContext, invoke)).resolves.toEqual(ids)
        expect(invoke).toHaveBeenCalledTimes(1)
        expect(invoke).toHaveBeenCalledWith('tokenize_batch', {
            request: {
                tokenizer_id: 'cl100k_base',
                artifact_fingerprint: NATIVE_TOKENIZER_FINGERPRINTS.cl100k_base,
                mode: 'ids',
                texts,
            },
        })
    })

    it('sends a model family batch with its artifact fingerprint and the texts unchanged', async () => {
        const texts = Array.from({ length: 4 }, (_, index) => `모델 ${index} 🙂`)
        const ids = texts.map((_, index) => [index])
        const invoke = vi.fn(async () => ({
            mode: 'ids',
            artifact_fingerprint: NATIVE_MODEL_TOKENIZER_FINGERPRINTS.claude,
            ids,
        }))

        await expect(
            tryNativeTokenizerIdsBatch(candidate(texts), { ...cl100kContext, modelTokenizerId: 'claude' }, invoke),
        ).resolves.toEqual(ids)
        expect(invoke).toHaveBeenCalledWith('tokenize_batch', {
            request: {
                tokenizer_id: 'claude',
                artifact_fingerprint: NATIVE_MODEL_TOKENIZER_FINGERPRINTS.claude,
                mode: 'ids',
                texts,
            },
        })
    })

    it('propagates a refused text and keeps the family native', async () => {
        const refusal = { code: 'disallowed_special_token', message: 'synthetic refusal', index: 0 }
        const invoke = vi.fn().mockRejectedValueOnce(refusal).mockResolvedValueOnce({
            mode: 'ids',
            artifact_fingerprint: NATIVE_TOKENIZER_FINGERPRINTS.o200k_base,
            ids: [[1]],
        })
        const context = { ...cl100kContext, modelTokenizerId: 'o200k_base' } as const

        await expect(tryNativeTokenizerIdsBatch(candidate(['<|endoftext|>']), context, invoke)).rejects.toBe(refusal)
        await expect(tryNativeTokenizerIdsBatch(candidate(['next']), context, invoke)).resolves.toEqual([[1]])
        expect(invoke).toHaveBeenCalledTimes(2)
    })

    it('propagates an availability failure and keeps the family in JavaScript afterwards', async () => {
        const failure = new Error('native boundary failed')
        const invoke = vi.fn(async () => {
            throw failure
        })
        const context = { ...cl100kContext, modelTokenizerId: 'glm4' } as const

        await expect(tryNativeTokenizerIdsBatch(candidate(['first']), context, invoke)).rejects.toBe(failure)
        await expect(tryNativeTokenizerIdsBatch(candidate(['second']), context, invoke)).resolves.toBeNull()
        expect(invoke).toHaveBeenCalledTimes(1)
    })
})
