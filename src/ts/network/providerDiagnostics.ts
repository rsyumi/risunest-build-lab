import { recordNativeLogError } from '../nativeLog'

const requestSequences = new Map<string, number>()
const providers: Record<string, string> = {
    'api.openai.com': 'openai', 'api.anthropic.com': 'anthropic',
    'generativelanguage.googleapis.com': 'google', 'aiplatform.googleapis.com': 'google',
    'openrouter.ai': 'openrouter', 'api.deepseek.com': 'deepseek',
    'api.groq.com': 'groq', 'api.mistral.ai': 'mistral', 'api.x.ai': 'xai',
    'ollama.com': 'ollama', 'api.novelai.net': 'novelai',
}

export async function observeProviderRequest<T extends { ok: boolean; status: number; transportFailure?: boolean; diagnosticStatus?: number }>(
    url: string,
    chatId: string | undefined,
    request: () => Promise<T>,
    signal?: AbortSignal,
): Promise<T> {
    if (!chatId) return request()
    let provider = 'custom'
    try {
        const hostname = new URL(url).hostname
        if (Object.hasOwn(providers, hostname)) provider = providers[hostname]
    } catch { /* No URL content enters diagnostics. */ }
    const key = JSON.stringify([chatId, provider])
    const requestSequence = (requestSequences.get(key) ?? 0) + 1
    requestSequences.delete(key)
    requestSequences.set(key, requestSequence)
    if (requestSequences.size > 128) requestSequences.delete(requestSequences.keys().next().value!)
    const failure = (status?: number) => {
        if (signal?.aborted) return
        const result = status === undefined ? 'transport-failure' : `http-status=${status}`
        void recordNativeLogError(`provider-request provider=${provider} request-sequence=${requestSequence} ${result}`).catch(() => {})
    }
    try {
        const response = await request()
        if (!response.ok) failure(response.transportFailure ? undefined : (response.diagnosticStatus ?? response.status))
        return response
    } catch (error) {
        failure()
        throw error
    }
}
