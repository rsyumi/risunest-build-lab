import type { Tokenizer } from '@mlc-ai/web-tokenizers'

let loadedTokenizer: Promise<Tokenizer> | undefined

async function loadModel(): Promise<ArrayBuffer> {
    const response = await fetch('/token/gemma/gemma4.json')
    if (!response.ok) {
        throw new Error(`Failed to load the Gemma 4 tokenizer (HTTP ${response.status}).`)
    }
    return response.arrayBuffer()
}

export async function encodeGemma(text: string) {
    if (!loadedTokenizer) {
        const loaded = (async () => {
            const { Tokenizer } = await import('@mlc-ai/web-tokenizers')
            return await Tokenizer.fromJSON(await loadModel())
        })()
        loadedTokenizer = loaded
        // Concurrent callers share initialization; a failed attempt can be retried.
        void loaded.catch(() => {
            if (loadedTokenizer === loaded) loadedTokenizer = undefined
        })
    }
    return (await loadedTokenizer).encode(text)
}
