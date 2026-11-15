// Writes the case files the Google oracle (`providers-google.oracle.test.ts`) reads under
// tests/fixtures/rust-core/providers/google. The cases are deterministic and use synthetic keys only: the service-account
// key is a throwaway RSA key kept in synthetic-vertex-key.pem. Run `node benchmarks/rust-core-oracles/providers-google.cases.mjs`
// and then the oracle with RUST_CORE_ORACLE_WRITE=1 to refresh the expectations.
import { mkdirSync, readFileSync, writeFileSync } from 'node:fs'
import { dirname, resolve } from 'node:path'
import { fileURLToPath } from 'node:url'

const defaultOut = resolve(dirname(fileURLToPath(import.meta.url)), '../../tests/fixtures/rust-core/providers/google')
const out = process.argv[2] ?? defaultOut
mkdirSync(out, { recursive: true })
const KEY = readFileSync(resolve(out, 'synthetic-vertex-key.pem'), 'utf8')

// LLMFlags numbers (src/ts/model/types.ts); custom flags replace the flags of the selected model.
const F = { hasImageInput: 0, hasImageOutput: 1, hasAudioInput: 2, hasAudioOutput: 3, hasFirstSystemPrompt: 7, hasStreaming: 8, requiresAlternateRole: 9, hasVideoInput: 12, geminiThinking: 15, geminiBlockOff: 16, noCivilIntegrity: 20, geminiThinkingNoMinimal: 26 }
const flags = (...names) => ({ enableCustomFlags: true, customFlags: names.map(name => F[name]) })

const sys = text => ({ role: 'system', content: text })
const user = (text, extra = {}) => ({ role: 'user', content: text, ...extra })
const bot = (text, extra = {}) => ({ role: 'assistant', content: text, ...extra })
const media = (type, base64) => ({ type, base64 })
const chat = [sys('Be brief.'), user('Hello')]
const talk = [sys('You are a narrator.'), user('Open the door.'), bot('The door creaks open.'), user('Walk in.')]

const clockAt = '2026-01-02T03:04:05Z'
const clockMs = Date.parse(clockAt)
const EMAIL = 'synthetic@example-project.iam.gserviceaccount.com'

const baseDb = {
    usePlainFetch: true,
    requestRetrys: 0,
    genTime: 1,
    useStreaming: false,
    applyAdditionalParamsToAll: false,
    additionalParams: [],
    maxResponse: 300,
    temperature: 80,
    top_k: 40,
    repetition_penalty: 1.1,
    min_p: 0.05,
    top_a: 0.2,
    top_p: 0.9,
    frequencyPenalty: 20,
    PresensePenalty: 30,
    thinkingTokens: 1024,
    reasoningEffort: 1,
    verbosity: 1,
    jsonSchemaEnabled: false,
    jsonSchema: 'answer',
    extractJson: '',
    seperateParametersEnabled: false,
    seperateParametersByModel: false,
    simplifiedToolUse: false,
    antiServerOverloads: false,
    streamGeminiThoughts: false,
    saveSignatures: false,
    gptVisionQuality: 'low',
    systemContentReplacement: '',
    systemRoleReplacement: '',
    forceReplaceUrl: '',
    proxyKey: 'proxy-key-synthetic',
    customProxyRequestModel: 'gemini-2.5-pro',
    customAPIFormat: 5,
    enableCustomFlags: false,
    customFlags: [],
    localNetworkMode: false,
    google: { accessToken: 'AIza-synthetic_key', projectId: 'proj-synthetic' },
    vertexRegion: 'us-central1',
    vertexClientEmail: EMAIL,
    vertexPrivateKey: KEY,
    vertexAccessToken: 'ya29.cached-synthetic',
    vertexAccessTokenExpires: clockMs + 3_600_000,
}

const separate = {
    seperateParametersEnabled: true,
    seperateParameters: {
        model: { temperature: 50, top_k: 10, repetition_penalty: 1, min_p: 0, top_a: 0, top_p: 0.5, frequency_penalty: 10, presence_penalty: 10, thinking_tokens: 2048, reasoning_effort: 0, verbosity: 0 },
        submodel: { temperature: 60, top_k: 0, repetition_penalty: 1, min_p: 0, top_a: 0, top_p: 0.6, frequency_penalty: 11, presence_penalty: 11, thinking_tokens: 0, reasoning_effort: 0, verbosity: 0 },
        memory: { temperature: 61, top_k: 11, repetition_penalty: 1, min_p: 0, top_a: 0, top_p: 0.6, frequency_penalty: 11, presence_penalty: 11, thinking_tokens: 0, reasoning_effort: 0, verbosity: 0 },
        emotion: { temperature: 62, top_k: 11, repetition_penalty: 1, min_p: 0, top_a: 0, top_p: 0.6, frequency_penalty: 11, presence_penalty: 11, thinking_tokens: 0, reasoning_effort: 0, verbosity: 0 },
        translate: { temperature: 63, top_k: 11, repetition_penalty: 1, min_p: 0, top_a: 0, top_p: 0.6, frequency_penalty: 11, presence_penalty: 11, thinking_tokens: 0, reasoning_effort: 0, verbosity: 0 },
        otherAx: { temperature: 70, top_k: 12, repetition_penalty: 1, min_p: 0, top_a: 0, top_p: 0.7, frequency_penalty: 12, presence_penalty: 12, thinking_tokens: 0, reasoning_effort: 0, verbosity: 0 },
        overrides: { 'gemini-2.5-pro': { temperature: 30, top_k: 5, repetition_penalty: 1, min_p: 0, top_a: 0, top_p: 0.3, frequency_penalty: 5, presence_penalty: 5, thinking_tokens: 4096, reasoning_effort: 0, verbosity: 0 } },
    },
}

const customModel = (extra = {}) => ({ id: 'xcustom:::one', name: 'One', internalId: 'gemini-proxy-model', url: 'https://llm.example.test/v1beta', key: 'custom-key-synthetic', format: 5, flags: [F.hasFirstSystemPrompt, F.hasStreaming], tokenizer: 0, params: '', ...extra })

const lookup = { name: 'lookup', description: 'Looks a word up.', inputSchema: { type: 'object', properties: { query: { type: 'string', description: 'The word.' } }, required: ['query'] } }
const clock = { name: 'clock', description: 'Tells the time.', inputSchema: { type: 'object', properties: { zone: { type: ['string', 'null'] } } } }

const G = 'gemini-2.5-pro'
const V = 'gemini-2.5-pro-vertex'
const tokenOk = { json: { access_token: 'ya29.minted-synthetic', expires_in: 3599, token_type: 'Bearer' } }
const expired = { vertexAccessTokenExpires: clockMs - 1000 }

function writeRequests() {
    const cases = []
    const add = (id, entry) => cases.push({ id, model: G, clock: clockAt, ...entry })

    // Layout, system handling and message merging.
    add('basic', { messages: chat })
    add('vertex-basic', { model: V, messages: chat })
    add('stream', { stream: true, messages: chat })
    add('vertex-stream', { model: V, stream: true, messages: chat })
    add('max-tokens', { maxTokens: 777, messages: chat })
    add('no-system', { messages: [user('Hello')] })
    add('multi-turn', { messages: talk })
    add('system-only', { messages: [sys('Only a system prompt.')] })
    add('empty-messages-throws', { messages: [] })
    add('system-prompt-keeps-whitespace', { messages: [sys('  padded  '), user('Hello')] })
    add('empty-system-prompt-is-dropped', { messages: [sys(''), user('Hello')] })
    add('consecutive-users-merge', { messages: [sys('Be brief.'), user('One'), user('Two'), user('Three')] })
    add('consecutive-assistants-merge', { messages: [sys('Be brief.'), user('One'), bot('Two'), bot('Three'), user('Four')] })
    add('system-after-user-merges', { messages: [sys('Be brief.'), user('One'), sys('Reminder'), bot('Two')] })
    add('system-after-assistant-becomes-user', { messages: [sys('Be brief.'), user('One'), bot('Two'), sys('Reminder'), user('Three')] })
    add('system-first-after-leading-system', { messages: [sys('First.'), sys('Second.'), user('Hello')] })
    add('function-role-becomes-user', { messages: [sys('Be brief.'), user('Hi'), { role: 'function', content: 'result', name: 'lookup' }, bot('Done')] })
    add('assistant-first', { messages: [sys('Be brief.'), bot('I begin.'), user('Hello')] })
    add('empty-content-message', { messages: [sys('Be brief.'), user('')] })
    add('system-content-replacement', { db: { systemContentReplacement: '[{{slot}}]', systemRoleReplacement: 'user' }, messages: [sys('Be brief.'), user('Hello')] })
    add('unicode-content', { messages: [sys('Be brief.'), user('Café 😀   end')] })

    // Media.
    add('image', { messages: [sys('Be brief.'), user('Look.', { multimodals: [media('image', 'data:image/png;base64,AAAA')] })] })
    add('two-images', { messages: [sys('Be brief.'), user('Look.', { multimodals: [media('image', 'data:image/png;base64,AAAA'), media('image', 'data:image/jpeg;base64,BBBB')] })] })
    add('audio', { messages: [sys('Be brief.'), user('Listen.', { multimodals: [media('audio', 'data:audio/wav;base64,CCCC')] })] })
    add('video', { messages: [sys('Be brief.'), user('Watch.', { multimodals: [media('video', 'data:video/mp4;base64,DDDD')] })] })
    add('image-unsupported-model', { model: 'gemini-2.5-flash-image-preview', db: flags('hasFirstSystemPrompt'), messages: [sys('Be brief.'), user('Look.', { multimodals: [media('video', 'data:video/mp4;base64,DDDD')] })] })
    add('image-on-assistant', { messages: [sys('Be brief.'), user('Hi'), bot('Here.', { multimodals: [media('image', 'data:image/png;base64,AAAA')] })] })
    add('image-without-comma', { messages: [sys('Be brief.'), user('Look.', { multimodals: [media('image', 'data:image/png;base64')] })] })
    add('image-without-colon', { messages: [sys('Be brief.'), user('Look.', { multimodals: [media('image', 'png;base64,AAAA')] })] })
    add('image-three-commas', { messages: [sys('Be brief.'), user('Look.', { multimodals: [media('image', 'data:image/png;base64,AA,AA')] })] })
    add('media-on-system-is-model-role', { messages: [sys('Be brief.'), user('Hi'), sys('Note.', { multimodals: [media('image', 'data:image/png;base64,AAAA')] })] })
    add('media-chat-merges-with-next', { messages: [sys('Be brief.'), user('Look.', { multimodals: [media('image', 'data:image/png;base64,AAAA')] }), user('Again.')] })
    add('signature-ignored-without-setting', { messages: [sys('Be brief.'), user('Hi', { multimodals: [media('signature', 'e30=')] })] })
    add('signature-with-setting-declines', { db: { saveSignatures: true }, messages: [sys('Be brief.'), user('Hi', { multimodals: [media('signature', 'e30=')] })], declines: 'signature-multimodal' })

    // Tool-call markers inside message content.
    const call = (id, name, arg) => ({ call: { id, name, arg }, response: [{ type: 'text', text: `${name} done` }] })
    const withTools = (messages, decodings, extra = {}) => ({ tools: [lookup, clock], messages, decodings, ...extra })
    add('tools-declared', { tools: [lookup, clock], messages: chat })
    add('marker-decoded', withTools([sys('Be brief.'), user('Go.'), bot('Before <tool_call>one</tool_call> after'), user('Next.')], { one: call('t1', 'lookup', { query: 'x' }) }))
    add('marker-only-text', withTools([sys('Be brief.'), user('Go.'), bot('<tool_call>one</tool_call>'), user('Next.')], { one: call('t1', 'lookup', { query: 'x' }) }))
    add('marker-two', withTools([sys('Be brief.'), user('Go.'), bot('A<tool_call>one</tool_call>B<tool_call>two</tool_call>C'), user('Next.')], { one: call('t1', 'lookup', { query: 'x' }), two: call('t2', 'clock', {}) }))
    add('marker-unknown-tool', withTools([sys('Be brief.'), user('Go.'), bot('A<tool_call>one</tool_call>B'), user('Next.')], { one: call('t1', 'missing', {}) }))
    add('marker-not-decoded', withTools([sys('Be brief.'), user('Go.'), bot('A<tool_call>zzz</tool_call>B'), user('Next.')], {}))
    add('marker-no-tools-declared', { messages: [sys('Be brief.'), user('Go.'), bot('A<tool_call>one</tool_call>B'), user('Next.')], decodings: { one: call('t1', 'lookup', {}) } })
    add('marker-unclosed', withTools([sys('Be brief.'), user('Go.'), bot('A<tool_call>one'), user('Next.')], { one: call('t1', 'lookup', {}) }))
    add('marker-across-line', withTools([sys('Be brief.'), user('Go.'), bot('A<tool_call>o\nne</tool_call>B'), user('Next.')], { 'o\nne': call('t1', 'lookup', {}) }))
    add('marker-in-user-message', withTools([sys('Be brief.'), user('Go <tool_call>one</tool_call>'), user('Next.')], { one: call('t1', 'lookup', { query: 'x' }) }))
    add('marker-first-chat-only', withTools([sys('Be brief.'), bot('<tool_call>one</tool_call>')], { one: call('t1', 'lookup', {}) }))
    add('marker-whitespace-segments', withTools([sys('Be brief.'), user('Go.'), bot('  A  <tool_call>one</tool_call>   <tool_call>two</tool_call>  '), user('Next.')], { one: call('t1', 'lookup', {}), two: call('t2', 'clock', {}) }))
    add('marker-in-media-chat-drops-media', withTools([sys('Be brief.'), user('Go.'), bot('<tool_call>one</tool_call>', { multimodals: [media('image', 'data:image/png;base64,AAAA')] }), user('Next.')], { one: call('t1', 'lookup', {}) }))
    add('marker-with-media-and-text', withTools([sys('Be brief.'), user('Go.'), bot('Lead <tool_call>one</tool_call>', { multimodals: [media('image', 'data:image/png;base64,AAAA')] }), user('Next.')], { one: call('t1', 'lookup', {}) }))
    add('marker-response-image-ignored', withTools([sys('Be brief.'), user('Go.'), bot('<tool_call>one</tool_call>'), user('Next.')], { one: { call: { id: 't1', name: 'lookup', arg: {} }, response: [{ type: 'image', mimeType: 'image/png', data: 'AAAA' }, { type: 'text', text: 'ok' }] } }))
    add('marker-merges-with-following-user', withTools([sys('Be brief.'), user('Go.'), bot('<tool_call>one</tool_call>'), user('Next.'), user('And.')], { one: call('t1', 'lookup', {}) }))

    // Safety settings.
    add('block-off-model', { model: 'gemini-2.0-flash', messages: chat })
    add('no-civil-integrity-model', { model: 'gemini-2.0-pro-exp', messages: chat })
    add('block-off-and-no-civil', { model: 'gemini-2.0-flash-lite-preview-02-05', messages: chat })

    // Parameters and thinking.
    add('thinking-tokens', { messages: chat })
    add('thinking-tokens-zero', { db: { thinkingTokens: 0 }, messages: chat })
    add('top-k-zero-is-ignored', { db: { top_k: 0 }, messages: chat })
    add('penalties-unset', { db: { frequencyPenalty: -1000, PresensePenalty: -1000, temperature: -1000 }, messages: chat })
    add('separate-parameters-model', { db: separate, messages: chat })
    add('separate-parameters-submodel', { db: separate, mode: 'submodel', messages: chat })
    add('separate-parameters-by-model', { db: { ...separate, seperateParametersByModel: true }, messages: chat })
    add('separate-parameters-by-model-missing', { model: 'gemini-2.0-flash', db: { ...separate, seperateParametersByModel: true }, messages: chat })
    for (const effort of [-1, 0, 1, 2, 3]) add(`effort-${effort}-3-pro`, { model: 'gemini-3-pro-preview', db: { reasoningEffort: effort }, messages: chat })
    for (const effort of [-1, 0, 2]) add(`effort-${effort}-no-minimal`, { model: 'gemini-3.1-pro-preview', db: { reasoningEffort: effort }, messages: chat })
    add('effort-only-flash-latest', { model: 'gemini-flash-latest', messages: chat })
    add('effort-flash-3-6', { model: 'gemini-3.6-flash', messages: chat })
    add('thinking-model-without-thinking-parameters', { model: 'gemini-2.0-flash-thinking-exp-01-21', messages: chat })
    add('non-thinking-model', { model: 'gemini-2.0-flash', messages: chat })
    add('vision-high', { db: { gptVisionQuality: 'high' }, messages: chat })
    add('image-output-model-never-streams', { model: 'gemini-2.5-flash-image', stream: true, messages: chat })
    add('image-output-vertex', { model: 'gemini-2.5-flash-image-vertex', stream: true, messages: chat })
    add('image-response-argument', { imageResponse: true, stream: true, messages: chat })
    add('audio-output-flag', { model: 'xcustom:::one', customModels: [customModel({ flags: [F.hasFirstSystemPrompt, F.hasStreaming, F.hasAudioOutput] })], stream: true, messages: chat })
    add('image-and-audio-output', { model: 'xcustom:::one', customModels: [customModel({ flags: [F.hasFirstSystemPrompt, F.hasStreaming, F.hasAudioOutput, F.hasImageOutput] })], stream: true, messages: chat })
    add('tools-and-thinking', { tools: [lookup], messages: chat })
    add('json-schema-enabled-declines', { db: { jsonSchemaEnabled: true }, messages: chat, declines: 'json-schema' })
    add('schema-argument-declines', { schema: 'answer', messages: chat, declines: 'json-schema' })

    // Additional parameters.
    const extra = (pairs, entry = {}) => ({ db: { applyAdditionalParamsToAll: true, additionalParams: pairs }, messages: chat, ...entry })
    add('additional-body-key', extra([['generation_config.candidateCount', '2']]))
    add('additional-header', extra([['header::x-synthetic', 'value']]))
    add('additional-header-authorization-vertex', { model: V, ...extra([['header::Authorization', 'Bearer other']]) })
    add('additional-none-deletes', extra([['safetySettings', '{{none}}']]))
    add('additional-json', extra([['generation_config.stopSequences', 'json::["END"]']]))
    add('additional-model-override', extra([['contents', 'json::[]']]))
    add('additional-not-applied-without-flag', { db: { additionalParams: [['top_k', '5']] }, messages: chat })
    add('additional-tools-after-undefined', extra([['tools', 'json::{"codeExecution":{}}']]))

    // Endpoints.
    add('region-us-west1', { model: V, db: { vertexRegion: 'us-west1' }, messages: chat })
    add('region-global-setting', { model: V, db: { vertexRegion: 'global' }, messages: chat })
    add('region-empty', { model: V, db: { vertexRegion: '' }, messages: chat })
    add('global-only-3-pro-preview', { model: 'gemini-3-pro-preview-vertex', messages: chat })
    add('global-only-3-flash-preview', { model: 'gemini-3-flash-preview-vertex', stream: true, messages: chat })
    add('global-only-3-7-flash', { model: 'gemini-3.7-flash-vertex', messages: chat })
    add('global-only-3-5-flash-lite', { model: 'gemini-3.5-flash-lite-vertex', messages: chat })
    for (const id of ['gemini-3.8-flash', 'gemini-3.9-flash', 'gemini-3.4-flash']) {
        add(`global-only-custom-${id.replace(/\./g, '-')}`, { model: 'xcustom:::one', customModels: [customModel({ internalId: id, format: 6, url: undefined })], messages: chat })
    }
    add('global-only-3-1-pro-preview-is-not-matched', { model: 'gemini-3.1-pro-preview-vertex', messages: chat })
    add('project-with-unsafe-character-declines', { model: V, db: { google: { accessToken: 'k', projectId: 'a/b' } }, messages: chat, declines: 'google-url-form' })
    add('region-with-unsafe-character-declines', { model: V, db: { vertexRegion: 'us central' }, messages: chat, declines: 'google-url-form' })
    add('key-from-database', { db: { google: { accessToken: 'AIza-other_key', projectId: 'p' } }, messages: chat })
    add('key-with-ampersand-declines', { db: { google: { accessToken: 'a&b', projectId: 'p' } }, messages: chat, declines: 'google-key-form' })
    add('key-with-space-declines', { db: { google: { accessToken: 'a b', projectId: 'p' } }, messages: chat, declines: 'google-key-form' })
    add('key-with-tilde', { db: { google: { accessToken: 'a~b*c', projectId: 'p' } }, messages: chat })
    add('empty-key', { db: { google: { accessToken: '', projectId: 'p' } }, messages: chat })
    add('custom-model-url', { model: 'xcustom:::one', customModels: [customModel()], messages: chat })
    add('custom-model-url-stream', { model: 'xcustom:::one', stream: true, customModels: [customModel()], messages: chat })
    add('custom-model-url-trailing-slash', { model: 'xcustom:::one', customModels: [customModel({ url: 'https://llm.example.test/v1beta/' })], messages: chat })
    add('custom-model-url-port-and-case', { model: 'xcustom:::one', customModels: [customModel({ url: 'HTTPS://LLM.Example.test:443/v1beta' })], messages: chat })
    add('custom-model-key-encoded', { model: 'xcustom:::one', customModels: [customModel({ key: 'a b&c=d/é*' })], messages: chat })
    add('custom-model-empty-key-falls-back', { model: 'xcustom:::one', customModels: [customModel({ key: '' })], messages: chat })
    add('custom-model-empty-url-uses-default', { model: 'xcustom:::one', customModels: [customModel({ url: '' })], messages: chat })
    add('custom-model-url-with-query-declines', { model: 'xcustom:::one', customModels: [customModel({ url: 'https://llm.example.test/v1beta?x=1' })], messages: chat, declines: 'google-custom-url' })
    add('custom-model-url-with-fragment-declines', { model: 'xcustom:::one', customModels: [customModel({ url: 'https://llm.example.test/v1beta#x' })], messages: chat, declines: 'google-custom-url' })
    add('custom-model-vertex-format', { model: 'xcustom:::one', customModels: [customModel({ format: 6 })], messages: chat })
    add('custom-model-vertex-with-url', { model: 'xcustom:::one', customModels: [customModel({ format: 6, url: 'https://llm.example.test/v1beta' })], messages: chat })
    add('custom-model-unsafe-id-declines', { model: 'xcustom:::one', customModels: [customModel({ internalId: 'gem ini/x' })], messages: chat, declines: 'google-custom-url' })
    add('custom-model-params', { model: 'xcustom:::one', customModels: [customModel({ params: 'generation_config.candidateCount=3\nheader::x-custom=yes' })], messages: chat })
    add('reverse-proxy', { model: 'reverse_proxy', db: { forceReplaceUrl: 'https://proxy.example.test/gemini' }, messages: chat })
    add('reverse-proxy-vertex', { model: 'reverse_proxy', db: { customAPIFormat: 6, forceReplaceUrl: 'https://proxy.example.test/gemini' }, messages: chat })
    add('reverse-proxy-no-url', { model: 'reverse_proxy', messages: chat })

    // The Vertex access token.
    add('token-cached', { model: V, messages: chat })
    add('token-expiry-equals-now-is-cached', { model: V, db: { vertexAccessTokenExpires: clockMs }, messages: chat })
    add('token-minted', { model: V, db: expired, mint: [tokenOk], messages: chat })
    add('token-minted-stream', { model: V, db: expired, stream: true, mint: [tokenOk], messages: chat })
    add('token-minted-literal-backslash-n-key', { model: V, db: { ...expired, vertexPrivateKey: KEY.trim().split('\n').join('\\n') }, mint: [tokenOk], messages: chat })
    add('token-minted-crlf-key', { model: V, db: { ...expired, vertexPrivateKey: KEY.replace(/\n/g, '\r\n') }, mint: [tokenOk], messages: chat })
    add('token-minted-key-with-padding-spaces', { model: V, db: { ...expired, vertexPrivateKey: KEY.trim().split('\n').join(' ') }, mint: [tokenOk], messages: chat })
    add('token-not-needed-for-google-cloud-format', { db: expired, messages: chat })
    add('token-missing-email', { model: V, db: { ...expired, vertexClientEmail: '' }, messages: chat })
    add('token-missing-key', { model: V, db: { ...expired, vertexPrivateKey: '' }, messages: chat })
    add('token-invalid-email', { model: V, db: { ...expired, vertexClientEmail: 'someone@example.com' }, messages: chat })
    add('token-invalid-key-markers', { model: V, db: { ...expired, vertexPrivateKey: 'not a key' }, messages: chat })
    add('token-missing-end-marker', { model: V, db: { ...expired, vertexPrivateKey: '-----BEGIN PRIVATE KEY-----\nAAAA' }, messages: chat })
    add('token-key-not-base64-declines', { model: V, db: { ...expired, vertexPrivateKey: '-----BEGIN PRIVATE KEY-----\n@@@@\n-----END PRIVATE KEY-----' }, mint: [tokenOk], messages: chat, declines: 'vertex-key-base64' })
    add('token-key-bad-length-declines', { model: V, db: { ...expired, vertexPrivateKey: '-----BEGIN PRIVATE KEY-----\nAAAAA\n-----END PRIVATE KEY-----' }, mint: [tokenOk], messages: chat, declines: 'vertex-key-base64' })
    add('token-key-not-der-declines', { model: V, db: { ...expired, vertexPrivateKey: '-----BEGIN PRIVATE KEY-----\nAAAA\n-----END PRIVATE KEY-----' }, mint: [tokenOk], messages: chat, declines: 'vertex-key-import' })
    add('token-status-400-json', { model: V, db: expired, mint: [{ status: 400, json: { error: 'invalid_grant', error_description: 'Bad.' } }], messages: chat })
    add('token-status-500-text', { model: V, db: expired, mint: [{ status: 500, text: 'oops' }], messages: chat })
    add('token-status-302-empty', { model: V, db: expired, mint: [{ status: 302, text: '' }], messages: chat })
    add('token-status-200-not-json', { model: V, db: expired, mint: [{ text: 'not json' }], messages: chat })
    add('token-status-200-empty', { model: V, db: expired, mint: [{ text: '' }], messages: chat })
    add('token-reply-null', { model: V, db: expired, mint: [{ json: null }], messages: chat })
    add('token-reply-array', { model: V, db: expired, mint: [{ json: [] }], messages: chat })
    add('token-reply-without-token', { model: V, db: expired, mint: [{ json: { expires_in: 1 } }], messages: chat })
    add('token-reply-empty-token', { model: V, db: expired, mint: [{ json: { access_token: '' } }], messages: chat })
    add('token-reply-null-token', { model: V, db: expired, mint: [{ json: { access_token: null } }], messages: chat })
    add('token-reply-zero-token', { model: V, db: expired, mint: [{ json: { access_token: 0 } }], messages: chat })
    add('token-reply-number-token-declines', { model: V, db: expired, mint: [{ json: { access_token: 5 } }], messages: chat, declines: 'vertex-token-not-text' })
    add('token-reply-bom', { model: V, db: expired, mint: [{ text: '﻿{"access_token":"ya29.bom"}' }], messages: chat })
    add('token-transport-error', { model: V, db: expired, mint: [{ throws: 'fetch failed' }], messages: chat })

    writeFileSync(`${out}/requests.cases.json`, JSON.stringify({ schema: 1, baseDb, cases }, null, 2) + '\n')
}

// Replies that are not streamed.
function writeResponses() {
    const cases = []
    const add = (id, entry) => cases.push({ id, model: G, clock: clockAt, messages: chat, ...entry })
    const parts = (...list) => ({ candidates: [{ content: { role: 'model', parts: list }, finishReason: 'STOP' }] })
    const t = (text, extra = {}) => ({ text, ...extra })
    const one = (...list) => ({ json: parts(...list) })
    const failure = (status, text) => ({ status, text })
    const exhausted = { status: 429, text: JSON.stringify({ error: { code: 429, message: 'Resource has been exhausted', status: 'RESOURCE_EXHAUSTED' } }) }

    add('text', { replies: [one(t('Hello there.'))] })
    add('text-unicode', { replies: [one(t('日本語 \u{1F600} "quoted" \\ back'))] })
    add('two-text-parts-join-with-blank-line', { replies: [one(t('One.'), t('Two.'))] })
    add('empty-text-part-is-skipped', { replies: [one(t(''), t('Kept.'))] })
    add('thought-then-text', { replies: [one(t('Thinking.', { thought: true }), t('Answer.'))] })
    add('thoughts-join-with-blank-line', { replies: [one(t('A.', { thought: true }), t('B.', { thought: true }), t('C.'))] })
    add('thoughts-only', { replies: [one(t('Only thinking.', { thought: true }))] })
    add('thought-after-text-still-leads', { replies: [one(t('Answer.'), t('Late thought.', { thought: true }))] })
    add('thought-false-is-content', { replies: [one(t('Plain.', { thought: false }))] })
    add('thought-truthy-string-is-thought', { replies: [one(t('Odd.', { thought: 'yes' }), t('Then.'))] })
    add('thought-signature-without-setting-is-ignored', { replies: [one(t('Signed.', { thoughtSignature: 'c2ln' }))] })
    add('part-without-text-is-skipped', { replies: [one({ other: 1 }, t('Kept.'))] })
    add('extra-fields-ignored', { replies: [{ json: { ...parts(t('Fine.')), usageMetadata: { promptTokenCount: 3 }, modelVersion: 'x' } }] })
    add('usage-metadata-with-status-and-feedback', { replies: [{ json: { ...parts(t('Fine.')), usageMetadata: { totalTokenCount: 5 }, modelStatus: { a: 1 }, promptFeedback: { blockReason: 'NONE' } } }] })
    add('second-candidate-ignored', { replies: [{ json: { candidates: [{ content: { parts: [t('First.')] } }, { content: { parts: [t('Second.')] } }] } }] })
    add('array-reply-concatenates-parts', { replies: [{ json: [parts(t('One.')), parts(t('Two.', { thought: true })), parts(t('Three.'))] }] })
    add('array-reply-with-null-and-empty-items', { replies: [{ json: [null, {}, parts(t('Kept.'))] }] })
    add('array-reply-all-empty', { replies: [{ json: [{}, null] }] })
    add('vertex-text', { model: V, replies: [one(t('Hello from Vertex.'))] })
    add('bom-body', { replies: [{ text: '﻿' + JSON.stringify(parts(t('Bom.'))) }] })
    add('status-201-is-ok', { replies: [{ status: 201, json: parts(t('Created.')) }] })

    // Empty answers.
    add('no-candidates', { replies: [{ json: {} }] })
    add('empty-candidates', { replies: [{ json: { candidates: [] } }] })
    add('blocked-prompt', { replies: [{ json: { promptFeedback: { blockReason: 'SAFETY' }, usageMetadata: { promptTokenCount: 4 } } }] })
    add('candidate-without-content', { replies: [{ json: { candidates: [{ finishReason: 'SAFETY' }] } }] })
    add('empty-parts', { replies: [one()] })
    add('parts-null', { replies: [{ json: { candidates: [{ content: { parts: null } }] } }] })
    add('parts-empty-string', { replies: [{ json: { candidates: [{ content: { parts: '' } }] } }] })
    add('reply-null', { replies: [{ json: null }] })
    add('reply-number', { replies: [{ json: 5 }] })
    add('reply-string', { replies: [{ json: 'text' }] })
    add('error-body-with-200', { replies: [{ json: { error: { code: 400, message: 'bad' } } }] })
    add('empty-reply-echoes-numbers', { replies: [{ text: '{"x":1.0,"y":1e21,"z":0.1,"big":12345678901234567890,"s":"\\u00e9","t":[1,2,{"a":null}]}' }] })
    add('only-whitespace-text-is-kept', { replies: [one(t(' '))] })

    // Failures.
    add('status-400-json', { replies: [failure(400, JSON.stringify({ error: { message: 'API key not valid.' } }))] })
    add('status-500-text', { replies: [failure(500, 'upstream broke')] })
    add('status-500-empty', { replies: [failure(500, '')] })
    add('status-503-unicode', { replies: [failure(503, 'ごめん "quoted"\n ')] })
    add('status-429-exhausted', { replies: [exhausted] })
    add('status-429-exhausted-vertex', { model: V, replies: [exhausted] })
    add('status-429-exhausted-anti-overload-retries', { db: { antiServerOverloads: true }, replies: [exhausted, one(t('After retry.'))] })
    add('status-429-exhausted-anti-overload-retries-twice', { db: { antiServerOverloads: true }, replies: [exhausted, exhausted, one(t('After two.'))] })
    add('status-429-exhausted-anti-overload-vertex-no-retry', { model: V, db: { antiServerOverloads: true }, replies: [exhausted] })
    add('status-429-without-marker-no-retry', { db: { antiServerOverloads: true }, replies: [failure(429, 'slow down')] })
    add('status-200-not-json', { replies: [{ text: 'not json' }] })
    add('status-200-empty-body', { replies: [{ text: '' }] })
    add('status-200-html', { replies: [{ text: '<html></html>' }] })

    // Replies the native path leaves to the TypeScript one.
    add('null-part-throws', { replies: [{ json: { candidates: [{ content: { parts: [null] } }] } }] })
    add('text-number-declines', { replies: [one({ text: 5 })], declines: 'part-text-not-text' })
    add('text-object-declines', { replies: [one({ text: { a: 1 } })], declines: 'part-text-not-text' })
    add('parts-string-is-ignored', { replies: [{ json: { candidates: [{ content: { parts: 'abc' } }] } }] })
    add('parts-number-is-ignored', { replies: [{ json: { candidates: [{ content: { parts: 5 } }] } }] })
    add('parts-true-is-ignored', { replies: [{ json: { candidates: [{ content: { parts: true } }] } }] })
    add('parts-object-declines', { replies: [{ json: { candidates: [{ content: { parts: { a: 1 } } }] } }], declines: 'parts-not-array' })
    add('function-call-declines', {
        tools: [lookup], toolReplies: [[{ type: 'text', text: '{"found":true}' }]],
        replies: [one({ functionCall: { name: 'lookup', args: { query: 'x' } } }), one(t('Done.'))], declines: 'tool-round',
    })
    add('signature-with-setting-declines', { db: { saveSignatures: true }, replies: [one(t('Signed.', { thoughtSignature: 'c2ln' }))], declines: 'signature-response' })
    add('inline-audio-declines', { replies: [one(t('Hear.'), { inlineData: { mimeType: 'audio/wav', data: 'UklGRg==' } })], declines: 'inline-data-response' })

    writeFileSync(`${out}/responses.cases.json`, JSON.stringify({ schema: 1, baseDb, cases }, null, 2) + '\n')
}

// Event streams.
function writeStreams() {
    const cases = []
    const add = (id, entry) => cases.push({ id, model: G, clock: clockAt, stream: true, messages: chat, ...entry })
    const ev = value => `data: ${JSON.stringify(value)}\n\n`
    const part = (text, extra = {}) => ev({ candidates: [{ content: { role: 'model', parts: [{ text, ...extra }] } }] })
    const thought = text => part(text, { thought: true })
    const reply = (...chunks) => ({ chunks: chunks.map(utf8 => ({ utf8 })) })
    const exhausted = { status: 429, text: JSON.stringify({ error: { code: 429, status: 'RESOURCE_EXHAUSTED' } }) }
    const streamThoughts = { streamGeminiThoughts: true }

    add('text-one-chunk', { replies: [reply(part('Hello') + part(' world'))] })
    add('text-chunk-per-event', { replies: [reply(part('Hello'), part(' world'))] })
    add('text-split-mid-line', { replies: [reply('data: {"candidates":[{"content":{"parts":[{"te', 'xt":"Hi"}]}}]}\n\n', part('!'))] })
    add('text-chunk-ends-after-data-line', { replies: [reply(part('A').trimEnd() + '\n', '\n', part('B'))] })
    add('text-last-line-without-newline', { replies: [reply(part('A'), 'data: ' + JSON.stringify({ candidates: [{ content: { parts: [{ text: 'B' }] } }] }))] })
    add('text-crlf', { replies: [reply(part('A').replaceAll('\n', '\r\n'), part('B').replaceAll('\n', '\r\n'))] })
    add('data-without-space-ignored', { replies: [reply(part('A').replace('data: ', 'data:'), part('B'))] })
    add('non-data-lines-ignored', { replies: [reply(': comment\nevent: message\n' + part('A'), 'id: 1\n' + part('B'))] })
    add('data-line-padded-with-spaces', { replies: [reply('data:  ' + JSON.stringify({ candidates: [{ content: { parts: [{ text: 'A' }] } }] }) + '  \n\n', part('B'))] })
    add('empty-text-part-ignored', { replies: [reply(part(''), part('A'))] })
    add('part-without-text-ignored', { replies: [reply(ev({ candidates: [{ content: { parts: [{ other: 1 }] } }] }), part('A'))] })
    add('several-parts-in-one-event', { replies: [reply(ev({ candidates: [{ content: { parts: [{ text: 'A' }, { text: 'B' }] } }] }))] })
    add('second-candidate-ignored', { replies: [reply(ev({ candidates: [{ content: { parts: [{ text: 'A' }] } }, { content: { parts: [{ text: 'B' }] } }] }))] })
    add('unicode-text', { replies: [reply(part('日本語'), part(' \u{1F600}'))] })
    add('multibyte-split-between-chunks', { replies: [{ chunks: [{ hex: Buffer.from(part('A'), 'utf8').toString('hex') }, { hex: Buffer.from('data: {"candidates":[{"content":{"parts":[{"text":"', 'utf8').toString('hex') + 'e3' }, { hex: '8182' + Buffer.from('"}]}}]}\n\n', 'utf8').toString('hex') }] }] })
    add('bom-at-chunk-start', { replies: [{ chunks: [{ hex: 'efbbbf' + Buffer.from(part('A'), 'utf8').toString('hex') }, { hex: Buffer.from(part('B'), 'utf8').toString('hex') }] }] })
    add('empty-stream', { replies: [{ chunks: [] }] })
    add('empty-chunks', { replies: [reply('', part('A'), '')] })

    // Thoughts.
    add('thought-then-text', { replies: [reply(thought('Hmm. '), part('Answer.'))] })
    add('thought-then-text-one-chunk', { replies: [reply(thought('Hmm. ') + part('Answer.'))] })
    add('thought-only', { replies: [reply(thought('Only.'))] })
    add('thoughts-accumulate-without-separator', { replies: [reply(thought('A'), thought('B'), thought('C'), part('Done'))] })
    add('text-thought-text', { replies: [reply(part('A'), thought('B'), part('C'))] })
    add('thought-with-stream-thoughts-on', { db: streamThoughts, replies: [reply(thought('Hmm. '), part('Answer.'))] })
    add('thought-only-with-stream-thoughts-on', { db: streamThoughts, replies: [reply(thought('Only.'))] })
    add('thoughts-accumulate-with-stream-thoughts-on', { db: streamThoughts, replies: [reply(thought('A'), thought('B'), thought('C'), part('Done'))] })
    add('text-thought-text-with-stream-thoughts-on', { db: streamThoughts, replies: [reply(part('A'), thought('B'), part('C'))] })
    add('thought-false-is-content', { replies: [reply(part('A', { thought: false }))] })
    add('thought-signature-without-setting-is-ignored', { replies: [reply(part('A', { thoughtSignature: 'c2ln' }), part('B'))] })
    add('vertex-text', { model: V, replies: [reply(part('A'), thought('B'), part('C'))] })

    // Lines the reader cannot use.
    add('done-line-ends-the-snapshots', { replies: [reply(part('A'), 'data: [DONE]\n\n', part('B'))] })
    add('done-line-in-the-same-chunk', { replies: [reply(part('A') + 'data: [DONE]\n\n' + part('B'))] })
    add('done-line-last-without-newline', { replies: [reply(part('A'), 'data: [DONE]', part('B'))] })
    add('done-line-padded', { replies: [reply(part('A'), 'data:   [DONE]  \n\n', part('B'))] })
    add('malformed-json-ends-the-snapshots', { replies: [reply(part('A'), 'data: {"candidates":[{\n\n', part('B'), part('C'))] })
    add('malformed-last-line-is-retried', { replies: [reply(part('A'), 'data: {"candidates"', ':[]}\n\n', part('B'))] })
    add('data-null-ends-the-snapshots', { replies: [reply(part('A'), 'data: null\n\n', part('B'))] })
    add('data-number-and-string-and-array-are-fine', { replies: [reply(part('A'), 'data: 5\n\n', 'data: "text"\n\n', 'data: []\n\n', 'data: {}\n\n', part('B'))] })
    add('empty-data-line-ends-the-snapshots', { replies: [reply(part('A'), 'data: \n\n', part('B'))] })
    add('parts-null-is-fine', { replies: [reply(ev({ candidates: [{ content: { parts: null } }] }), part('A'))] })
    add('parts-string-is-fine', { replies: [reply(ev({ candidates: [{ content: { parts: 'abc' } }] }), part('A'))] })
    add('parts-empty-string-is-fine', { replies: [reply(ev({ candidates: [{ content: { parts: '' } }] }), part('A'))] })
    add('parts-number-ends-the-snapshots', { replies: [reply(part('A'), ev({ candidates: [{ content: { parts: 5 } }] }), part('B'))] })
    add('parts-object-ends-the-snapshots', { replies: [reply(part('A'), ev({ candidates: [{ content: { parts: { a: 1 } } }] }), part('B'))] })
    add('parts-true-ends-the-snapshots', { replies: [reply(part('A'), ev({ candidates: [{ content: { parts: true } }] }), part('B'))] })
    add('part-null-ends-the-snapshots', { replies: [reply(part('A'), ev({ candidates: [{ content: { parts: [null] } }] }), part('B'))] })
    add('part-primitives-are-fine', { replies: [reply(ev({ candidates: [{ content: { parts: [5, 'x', true, [], {}] } }] }), part('A'))] })
    add('candidates-empty-and-missing', { replies: [reply(ev({ candidates: [] }), ev({}), part('A'))] })
    add('candidates-object-with-zero-key', { replies: [reply(ev({ candidates: { 0: { content: { parts: [{ text: 'A' }] } } } }))] })
    add('text-number-declines', { replies: [reply(ev({ candidates: [{ content: { parts: [{ text: 5 }] } }] }))], declines: 'part-text-not-text' })
    add('text-after-text-object-declines', { replies: [reply(part('A'), ev({ candidates: [{ content: { parts: [{ text: { a: 1 } }] } }] }))], declines: 'part-text-not-text', terminal: true })

    // Metadata the owner hands to the registered interceptors.
    add('usage-metadata', { replies: [reply(ev({ candidates: [{ content: { parts: [{ text: 'A' }] } }], usageMetadata: { totalTokenCount: 5 } }), part('B'))] })
    add('usage-metadata-and-model-status', { replies: [reply(ev({ usageMetadata: { a: 1 }, modelStatus: { b: 2 } }), part('B'))] })

    // Replies to the stream request that are not a stream.
    add('status-500', { replies: [{ status: 500, text: 'upstream broke' }] })
    add('status-400-json', { replies: [{ status: 400, text: JSON.stringify({ error: { message: 'API key not valid.' } }) }] })
    add('status-500-empty', { replies: [{ status: 500, text: '' }] })
    add('status-201-is-not-a-stream', { replies: [{ status: 201, text: 'created' }] })
    add('status-429-exhausted', { replies: [exhausted] })
    add('status-429-exhausted-vertex', { model: V, replies: [exhausted] })
    add('status-429-exhausted-anti-overload-retries', { db: { antiServerOverloads: true }, replies: [exhausted, reply(part('After retry.'))] })
    add('status-429-exhausted-anti-overload-vertex-no-retry', { model: V, db: { antiServerOverloads: true }, replies: [exhausted] })
    add('status-429-without-marker-no-retry', { db: { antiServerOverloads: true }, replies: [{ status: 429, text: 'slow down' }] })
    add('status-500-bom', { replies: [{ status: 500, text: '﻿broken' }] })

    // Replies the native path leaves to the TypeScript one.
    add('function-call-after-text-declines', {
        tools: [lookup], toolReplies: [[{ type: 'text', text: '{"found":true}' }]],
        replies: [reply(part('A'), ev({ candidates: [{ content: { parts: [{ functionCall: { name: 'lookup', args: { query: 'x' } } }] } }] })), reply(part('Done.'))],
        declines: 'tool-round', terminal: true,
    })
    add('function-call-first-declines', {
        tools: [lookup], toolReplies: [[{ type: 'text', text: '{"found":true}' }]],
        replies: [reply(ev({ candidates: [{ content: { parts: [{ functionCall: { name: 'lookup', args: { query: 'x' } } }] } }] })), reply(part('Done.'))],
        declines: 'tool-round',
    })
    add('signature-with-setting-declines', { db: { saveSignatures: true }, replies: [reply(part('A', { thoughtSignature: 'c2ln' }))], declines: 'signature-response' })

    writeFileSync(`${out}/streams.cases.json`, JSON.stringify({ schema: 1, baseDb, cases }, null, 2) + '\n')
}

writeRequests()
writeResponses()
writeStreams()
