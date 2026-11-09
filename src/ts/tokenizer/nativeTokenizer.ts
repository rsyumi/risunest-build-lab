import { invoke } from '@tauri-apps/api/core'

export type NativeTokenizerId = 'cl100k_base' | 'o200k_base'
// The web tokenizers the native command runs over the artifact TS fetches, by their `encodeWithTokenizer` names.
export type NativeModelTokenizerId =
    | 'mistral'
    | 'llama'
    | 'novelai'
    | 'novellist'
    | 'claude'
    | 'llama3'
    | 'gemma'
    | 'cohere'
    | 'deepseek'
    | 'deepseek-v4'
    | 'glm4'
    | 'glm5'
export type NativeTokenizeMode = 'count' | 'ids'

export const NATIVE_TOKENIZER_FINGERPRINTS = {
    cl100k_base:
        'cl100k_base:dqbd-1.0.22:49a4e05dea02c8fafbd50cc4725c4aab8f39386c0afedef118e0dfebc2fe523a:tiktoken-rs-0.12.0:contract-1',
    o200k_base:
        'o200k_base:dqbd-1.0.22:a2c363f80642c0f07d916716b3940ff030121f3b5cef72b2c5bd1d4f64c14fb8:tiktoken-rs-0.12.0:contract-1',
} as const satisfies Record<NativeTokenizerId, string>

export const NATIVE_MODEL_TOKENIZER_FINGERPRINTS = {
    mistral:
        'mistral:web-tokenizers-0.1.6:dadfd56d766715c61d2ef780a525ab43b8e6da4de6865bda3d95fdef5e134055:risunest-tokenizer:contract-1',
    llama:
        'llama:web-tokenizers-0.1.6:9e556afd44213b6bd1be2b850ebbbd98f5481437a8021afaf58ee7fb1818d347:risunest-tokenizer:contract-1',
    novelai:
        'novelai:web-tokenizers-0.1.6:005ad680b10f1abd406bdb0ca9c6a5d83fc1f6e0a855bdd1942c1ceab1fb47ab:risunest-tokenizer:contract-1',
    novellist:
        'novellist:web-tokenizers-0.1.6:ec31a22c0dca80dc303894ca87547154961b22e312c10c6fab751db7c4584439:risunest-tokenizer:contract-1',
    claude:
        'claude:web-tokenizers-0.1.6:c241737df24b4e7f7c9af4fdcee29a0ca903dcb288a8b753bc346a3092911767:risunest-tokenizer:contract-1',
    llama3:
        'llama3:web-tokenizers-0.1.6:c05a3c2174e9edd5be19dc5a0748c42a9037bec2811ce062728bfd71f8702d78:risunest-tokenizer:contract-1',
    gemma:
        'gemma:web-tokenizers-0.1.6:4453a86456cdf02b3c8f79091ffda7ca017619ff5da495b9eda12141d5793962:risunest-tokenizer:contract-1',
    cohere:
        'cohere:web-tokenizers-0.1.6:5e07ab012ff6144f924acb0c46e8462ade9119d6375a7712a170dc7620291493:risunest-tokenizer:contract-1',
    deepseek:
        'deepseek:web-tokenizers-0.1.6:ecb6f9fc369894346f0511f4074ca75cee5cd5f3b06d02f1ba35fcd39f8e121d:risunest-tokenizer:contract-1',
    'deepseek-v4':
        'deepseek-v4:web-tokenizers-0.1.6:3ad46a8c78edf140516eb696f948ae6e7c3522ec64993b219833145e2e9951d0:risunest-tokenizer:contract-1',
    glm4:
        'glm4:web-tokenizers-0.1.6:1dfc31b811042be1dab7f489cd8e677eb014caa3f435d9543ae4807b27cb65ce:risunest-tokenizer:contract-1',
    glm5:
        'glm5:web-tokenizers-0.1.6:f73933d2e4b633ce5a75f049a68b339790c52b966025e895aae0a0eda67d5cda:risunest-tokenizer:contract-1',
} as const satisfies Record<NativeModelTokenizerId, string>

// General routing stays disabled. Measured production seams opt in explicitly.
export const NATIVE_TOKENIZER_PRODUCTION_ENABLED = false

export type ResolvedTokenizerRoute =
    | {
          kind: 'native-tiktoken'
          tokenizerId: NativeTokenizerId
          fingerprint: string
      }
    | {
          kind: 'native-model'
          tokenizerId: NativeModelTokenizerId
          fingerprint: string
      }
    | {
          kind: 'existing'
          tokenizerId: string
      }

export type NativeTokenizeBatchResponse =
    | { mode: 'count'; artifact_fingerprint: string; counts: number[] }
    | { mode: 'ids'; artifact_fingerprint: string; ids: number[][] }

export type NativeTokenizerInvoke = (
    command: string,
    args: Record<string, unknown>,
) => Promise<unknown>

export class NativeTokenizerBoundaryError extends Error {
    constructor(
        readonly code: string,
        message: string,
    ) {
        super(message)
        this.name = 'NativeTokenizerBoundaryError'
    }
}

function isNativeTokenizerId(tokenizerId: string): tokenizerId is NativeTokenizerId {
    return tokenizerId === 'cl100k_base' || tokenizerId === 'o200k_base'
}

export function isNativeModelTokenizerId(tokenizerId?: string): tokenizerId is NativeModelTokenizerId {
    return tokenizerId !== undefined && Object.hasOwn(NATIVE_MODEL_TOKENIZER_FINGERPRINTS, tokenizerId)
}

export function resolveNativeTokenizerRoute(
    tokenizerId: string,
    isTauri: boolean,
    candidateEnabled = NATIVE_TOKENIZER_PRODUCTION_ENABLED,
): ResolvedTokenizerRoute {
    if (!candidateEnabled || !isTauri) {
        return { kind: 'existing', tokenizerId }
    }
    if (isNativeModelTokenizerId(tokenizerId)) {
        return {
            kind: 'native-model',
            tokenizerId,
            fingerprint: NATIVE_MODEL_TOKENIZER_FINGERPRINTS[tokenizerId],
        }
    }
    if (!isNativeTokenizerId(tokenizerId)) {
        return { kind: 'existing', tokenizerId }
    }
    return {
        kind: 'native-tiktoken',
        tokenizerId,
        fingerprint: NATIVE_TOKENIZER_FINGERPRINTS[tokenizerId],
    }
}

// A high surrogate without its low half, or a low surrogate without its high half.
const LONE_SURROGATE = /[\ud800-\udbff](?![\udc00-\udfff])|(?:[^\ud800-\udbff]|^)[\udc00-\udfff]/

export function hasLoneSurrogate(text: string): boolean {
    return LONE_SURROGATE.test(text)
}

function normalizeUtf16(text: string): string {
    if (!LONE_SURROGATE.test(text)) return text
    let normalized = ''
    for (let index = 0; index < text.length; index++) {
        const codeUnit = text.charCodeAt(index)
        if (codeUnit >= 0xd800 && codeUnit <= 0xdbff) {
            const next = text.charCodeAt(index + 1)
            if (next >= 0xdc00 && next <= 0xdfff) {
                normalized += text[index] + text[index + 1]
                index++
            } else {
                normalized += '\ufffd'
            }
        } else if (codeUnit >= 0xdc00 && codeUnit <= 0xdfff) {
            normalized += '\ufffd'
        } else {
            normalized += text[index]
        }
    }
    return normalized
}

export async function invokeNativeTokenizerBatch(
    route: Exclude<ResolvedTokenizerRoute, { kind: 'existing' }>,
    texts: string[],
    mode: NativeTokenizeMode,
    invokeCommand: NativeTokenizerInvoke = invoke,
): Promise<NativeTokenizeBatchResponse> {
    // web-tokenizers reads a lone high surrogate together with the unit after it, which no well-formed text the
    // command receives can reproduce.
    if (route.kind === 'native-model' && texts.some(hasLoneSurrogate)) {
        throw new NativeTokenizerBoundaryError(
            'lone_surrogate',
            'The native tokenizer does not encode a lone surrogate the way web-tokenizers does.',
        )
    }
    const response = (await invokeCommand('tokenize_batch', {
        request: {
            tokenizer_id: route.tokenizerId,
            artifact_fingerprint: route.fingerprint,
            mode,
            texts: route.kind === 'native-model' ? texts : texts.map(normalizeUtf16),
        },
    })) as NativeTokenizeBatchResponse

    if (response?.artifact_fingerprint !== route.fingerprint) {
        throw new NativeTokenizerBoundaryError(
            'artifact_fingerprint_mismatch',
            'The native tokenizer returned a different artifact fingerprint.',
        )
    }
    if (response.mode !== mode) {
        throw new NativeTokenizerBoundaryError(
            'response_mode_mismatch',
            'The native tokenizer returned a different response mode.',
        )
    }
    const responseLength =
        response.mode === 'count' ? response.counts?.length : response.ids?.length
    if (responseLength !== texts.length) {
        throw new NativeTokenizerBoundaryError(
            'response_length_mismatch',
            'The native tokenizer returned a different number of batch items.',
        )
    }
    return response
}
