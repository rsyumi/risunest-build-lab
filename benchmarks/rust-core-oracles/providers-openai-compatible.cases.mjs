// Writes the case files the OpenAI-compatible oracle (`providers-openai-compatible.oracle.test.ts`) reads:
// `requests`, `responses` and `streams` under tests/fixtures/rust-core/providers/openai-compatible. The cases are
// deterministic and use synthetic keys only. Run `node benchmarks/rust-core-oracles/providers-openai-compatible.cases.mjs`
// and then the oracle with RUST_CORE_ORACLE_WRITE=1 to refresh the expectations.
import { mkdirSync, writeFileSync } from 'node:fs'
import { dirname, resolve } from 'node:path'
import { fileURLToPath } from 'node:url'

const defaultOut = resolve(dirname(fileURLToPath(import.meta.url)), '../../tests/fixtures/rust-core/providers/openai-compatible')
const out = process.argv[2] ?? defaultOut
mkdirSync(out, { recursive: true })

function writeRequests() {

  const F = { hasImageInput: 0, hasPrefill: 4, hasFullSystemPrompt: 6, hasFirstSystemPrompt: 7, hasStreaming: 8, requiresAlternateRole: 9, mustStartWithUserInput: 10, OAICompletionTokens: 13, DeveloperRole: 14, deepSeekPrefix: 17, deepSeekThinkingInput: 18, deepSeekThinkingOutput: 19, deepSeekThinkingToggle: 24, noStructuredOutput: 25 }
  const flags = (...names) => ({ enableCustomFlags: true, customFlags: names.map(name => F[name]) })

  const sys = text => ({ role: 'system', content: text })
  const user = text => ({ role: 'user', content: text })
  const bot = text => ({ role: 'assistant', content: text })
  const chat = [sys('Be brief.'), user('Hello')]
  const talk = [sys('You are a narrator.'), user('Open the door.'), bot('The door creaks open.'), user('Walk in.')]
  const image = base64 => ({ type: 'image', base64 })

  const baseDb = {
      usePlainFetch: true,
      openAIKey: 'sk-synthetic-openai',
      proxyKey: 'sk-synthetic-proxy',
      openrouterKey: 'sk-synthetic-openrouter',
      nanogptKey: 'sk-synthetic-nanogpt',
      OaiCompAPIKeys: { deepseek: 'sk-synthetic-deepseek', deepinfra: 'sk-synthetic-deepinfra' },
      requestRetrys: 0,
      genTime: 1,
      useStreaming: true,
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
      newOAIHandle: true,
      generationSeed: -1,
      gptVisionQuality: 'low',
      jsonSchemaEnabled: false,
      jsonSchema: '{"type":"object","properties":{"answer":{"type":"string"}},"required":["answer"]}',
      strictJsonSchema: true,
      extractJson: '',
      OAIPrediction: '',
      seperateParametersEnabled: false,
      seperateParametersByModel: false,
      simplifiedToolUse: false,
      reverseProxyOobaMode: false,
      reverseProxyOobaArgs: {},
      proxyRequestModel: '',
      customProxyRequestModel: '',
      openrouterRequestModel: 'openai/gpt-4o',
      openrouterFallback: false,
      openrouterMiddleOut: false,
      openrouterProvider: { order: [], only: [], ignore: [] },
      useInstructPrompt: false,
      deepseekThinkingType: 'disabled',
      autofillRequestUrl: false,
      openAIFlexProcessing: false,
      systemContentReplacement: '',
      systemRoleReplacement: '',
      forceReplaceUrl: '',
      customAPIFormat: 0,
      enableCustomFlags: false,
      customFlags: [],
      localNetworkMode: false,
  }

  const separate = {
      seperateParametersEnabled: true,
      seperateParameters: {
          model: { temperature: 50, top_k: 10, repetition_penalty: 1, min_p: 0, top_a: 0, top_p: 0.5, frequency_penalty: 10, presence_penalty: 10, thinking_tokens: 0, reasoning_effort: 0, verbosity: 0 },
          submodel: { temperature: 60, top_k: 11, repetition_penalty: 1, min_p: 0, top_a: 0, top_p: 0.6, frequency_penalty: 11, presence_penalty: 11, thinking_tokens: 0, reasoning_effort: 0, verbosity: 0 },
          memory: { temperature: 61, top_k: 11, repetition_penalty: 1, min_p: 0, top_a: 0, top_p: 0.6, frequency_penalty: 11, presence_penalty: 11, thinking_tokens: 0, reasoning_effort: 0, verbosity: 0 },
          emotion: { temperature: 62, top_k: 11, repetition_penalty: 1, min_p: 0, top_a: 0, top_p: 0.6, frequency_penalty: 11, presence_penalty: 11, thinking_tokens: 0, reasoning_effort: 0, verbosity: 0 },
          translate: { temperature: 63, top_k: 11, repetition_penalty: 1, min_p: 0, top_a: 0, top_p: 0.6, frequency_penalty: 11, presence_penalty: 11, thinking_tokens: 0, reasoning_effort: 0, verbosity: 0 },
          otherAx: { temperature: 70, top_k: 12, repetition_penalty: 1, min_p: 0, top_a: 0, top_p: 0.7, frequency_penalty: 12, presence_penalty: 12, thinking_tokens: 0, reasoning_effort: 0, verbosity: 0 },
          overrides: { gpt4o: { temperature: 30, top_k: 5, repetition_penalty: 1, min_p: 0, top_a: 0, top_p: 0.3, frequency_penalty: -1000, presence_penalty: 5, thinking_tokens: 0, reasoning_effort: 0, verbosity: 0 } },
      },
  }

  const customModel = (extra = {}) => ({ id: 'xcustom:::one', name: 'One', internalId: 'my-model', url: 'https://llm.example.test/v1/chat/completions', key: 'sk-synthetic-custom', format: 0, flags: [], tokenizer: 0, params: '', ...extra })

  const cases = []
  const add = (id, entry) => cases.push({ id, ...entry })

  // Plain OpenAI.
  add('gpt4o-basic', { model: 'gpt4o', messages: chat })
  add('gpt4o-stream', { model: 'gpt4o', stream: true, messages: chat })
  add('gpt4o-no-system', { model: 'gpt4o', messages: [user('Hello')] })
  add('gpt4o-multi-turn', { model: 'gpt4o', messages: talk })
  add('gpt4o-blank-message-dropped', { model: 'gpt4o', messages: [sys('Be brief.'), user(''), user('Hello')] })
  add('gpt4o-blank-message-kept-without-new-handle', { model: 'gpt4o', db: { newOAIHandle: false }, messages: [sys('Be brief.'), user(''), user('Hello')] })
  add('gpt4o-new-chat-memo-emptied', { model: 'gpt4o', messages: [sys('Be brief.'), { role: 'user', content: 'Start', memo: 'NewChat 1' }, user('Hello')] })
  add('gpt4o-new-chat-memo-kept-without-new-handle', { model: 'gpt4o', db: { newOAIHandle: false }, messages: [sys('Be brief.'), { role: 'user', content: 'Start', memo: 'NewChat 1' }, user('Hello')] })
  add('gpt4o-example-name-kept', { model: 'gpt4o', messages: [sys('Be brief.'), { role: 'user', content: 'Hi', name: 'example_user' }, { role: 'assistant', content: 'Hey', name: 'example_assistant' }, { role: 'user', content: 'Hello', name: 'Alice' }] })
  add('gpt4o-example-name-dropped-without-new-handle', { model: 'gpt4o', db: { newOAIHandle: false }, messages: [sys('Be brief.'), { role: 'user', content: 'Hi', name: 'example_user' }, user('Hello')] })
  add('gpt4o-function-role-keeps-fields', { model: 'gpt4o', messages: [sys('Be brief.'), { role: 'function', content: 'result', name: 'lookup', memo: 'm', removable: true }, user('Hello')] })
  add('gpt4o-extra-message-fields-dropped', { model: 'gpt4o', messages: [sys('Be brief.'), { role: 'user', content: 'Hello', memo: 'm', removable: true, attr: ['a'], thoughts: ['t'], cachePoint: true }] })
  add('gpt4o-max-tokens', { model: 'gpt4o', maxTokens: 777, messages: chat })
  add('gpt4o-sampling-disabled', { model: 'gpt4o', db: { temperature: -1000, frequencyPenalty: -1000, PresensePenalty: 0 }, messages: chat })
  add('gpt4o-separate-parameters-by-mode', { model: 'gpt4o', mode: 'memory', db: separate, messages: chat })
  add('gpt4o-separate-parameters-submodel', { model: 'gpt4o', mode: 'submodel', db: separate, messages: chat })
  add('gpt4o-separate-parameters-by-model', { model: 'gpt4o', db: { ...separate, seperateParametersByModel: true }, messages: chat })
  add('gpt4o-multigen', { model: 'gpt4o', db: { genTime: 3 }, messages: chat })
  add('gpt4o-multigen-continue', { model: 'gpt4o', continue: true, db: { genTime: 3 }, messages: chat })
  add('gpt4o-multigen-declined-by-argument', { model: 'gpt4o', noMultiGen: true, db: { genTime: 3 }, messages: chat })
  add('gpt4o-seed', { model: 'gpt4o', db: { generationSeed: 42 }, messages: chat })
  add('gpt4o-prediction', { model: 'gpt4o', db: { OAIPrediction: 'Hello there' }, messages: chat })
  add('gpt4o-flex', { model: 'gpt4o', db: { openAIFlexProcessing: true }, messages: chat })
  add('gpt4o-schema-argument', { model: 'gpt4o', schema: '{"type":"object","properties":{"answer":{"type":"string"},"score":{"type":"number"}},"required":["answer"]}', messages: chat })
  add('gpt4o-schema-from-database', { model: 'gpt4o', db: { jsonSchemaEnabled: true }, messages: chat })
  add('gpt4o-schema-not-strict', { model: 'gpt4o', db: { jsonSchemaEnabled: true, strictJsonSchema: false }, messages: chat })
  add('gpt4o-tools', {
      model: 'gpt4o', messages: chat,
      tools: [
          { name: 'lookup', description: 'Find a record', inputSchema: { type: 'object', properties: { query: { type: 'string', description: 'search text' }, limit: { type: 'number', maximum: 10 }, tags: { type: 'array', items: { type: 'string' } } }, required: ['query'], additionalProperties: false } },
          { name: 'ping', description: 'No arguments', inputSchema: { type: 'object' } },
      ],
  })
  add('gpt4o-tools-nullable-and-enum', {
      model: 'gpt4o', messages: chat,
      tools: [{ name: 'pick', description: 'Pick one', inputSchema: { type: 'object', properties: { color: { type: 'string', enum: ['red', 'green'], format: 'color' }, note: { type: ['string', 'null'] }, any: { anyOf: [{ type: 'string' }, { type: 'number' }] } } } }],
  })
  add('gpt4o-tools-with-multigen-fails', { model: 'gpt4o', db: { genTime: 2 }, messages: chat, tools: [{ name: 'ping', description: 'No arguments', inputSchema: { type: 'object' } }] })
  add('gpt4o-bias-record', { model: 'gpt4o', bias: { '50256': -100, '12': 5 }, messages: chat })
  add('gpt4o-bias-string-tokens', { model: 'gpt4o', biasString: [['hello', 3], ['bye', -2]], tokens: { hello: [15339, 10], bye: [1, 2000000] }, messages: chat })
  add('gpt4o-bias-bracket-id', { model: 'gpt4o', biasString: [['[[77]]', 4], ['[[x]]', 1]], messages: chat })
  add('gpt4o-bias-strong-ban', { model: 'gpt4o', biasString: [['banned', -101]], strong: { banned: [900, 9] }, messages: chat })
  add('gpt4o-bias-merges-with-record', { model: 'gpt4o', bias: { '9': 1 }, biasString: [['x', 2]], tokens: { x: [9, 3] }, messages: chat })
  add('gpt4o-bias-empty-token-list', { model: 'gpt4o', biasString: [['unknown', 2]], messages: chat })
  add('gpt4o-image-low', { model: 'gpt4o', messages: [sys('Look.'), { role: 'user', content: 'What is this?', multimodals: [image('data:image/png;base64,AAAA')] }] })
  add('gpt4o-image-high-two', { model: 'gpt4o', db: { gptVisionQuality: 'high' }, messages: [sys('Look.'), { role: 'user', content: 'Compare', multimodals: [image('data:image/png;base64,AAAA'), image('data:image/jpeg;base64,BBBB')] }] })
  add('gpt4o-image-on-assistant-ignored', { model: 'gpt4o', messages: [sys('Look.'), { role: 'assistant', content: 'See', multimodals: [image('data:image/png;base64,AAAA')] }, user('ok')] })
  add('gpt4o-tool-call-markers', {
      model: 'gpt4o', messages: [sys('Be brief.'), { role: 'assistant', content: 'Let me check.<tool_call>c1</tool_call>Done.' }, user('Thanks')],
      decodings: { c1: { call: { id: 'call_1', name: 'lookup', arg: '{"query":"x"}' }, response: [{ type: 'text', text: 'found' }] } },
  })
  add('gpt4o-tool-call-marker-undecoded', { model: 'gpt4o', messages: [sys('Be brief.'), { role: 'assistant', content: 'Before<tool_call>zzz</tool_call>After' }, user('Thanks')] })
  add('gpt4o-additional-params-all', {
      model: 'gpt4o', db: { applyAdditionalParamsToAll: true, additionalParams: [['top_logprobs', '3'], ['logprobs', 'true'], ['user', '"abc"'], ['metadata.tag', 'x'], ['empty', '{{none}}'], ['max_tokens', '12']] },
      messages: chat,
  })
  add('gpt4o-additional-params-json-and-header', {
      model: 'gpt4o', db: { applyAdditionalParamsToAll: true, additionalParams: [['header::X-Test', 'one'], ['json::response_format', '{"type":"json_object"}'], ['json::stop', "['a', 'b']"], ['json::flag', 'True'], ['nothing', 'null'], ['neg', '-1.5']] },
      messages: chat,
  })
  add('gpt4o-additional-params-not-applied', { model: 'gpt4o', db: { additionalParams: [['top_logprobs', '3']] }, messages: chat })
  add('gpt4o-additional-params-cannot-force-stream', { model: 'gpt4o', db: { applyAdditionalParamsToAll: true, additionalParams: [['stream', 'true']] }, messages: chat })
  add('gpt4o-additional-params-override-stream-when-streaming', { model: 'gpt4o', stream: true, db: { applyAdditionalParamsToAll: true, additionalParams: [['stream', 'false']] }, messages: chat })
  add('gpt4o-unicode-content', { model: 'gpt4o', messages: [sys('한국어로 답하세요 \u{1F600}'), user('안녕하세요 😀 "quote" \\ back\nline\ttab \u0007')] })
  add('gpt4o-lone-surrogate-content', { model: 'gpt4o', messages: [sys('Be brief.'), user('lone \ud800 high and \udc00 low')] })

  // Flags on an unlisted model id.
  add('flag-developer-role', { model: 'gpt4o', db: flags('hasFullSystemPrompt', 'DeveloperRole'), messages: chat })
  add('flag-completion-tokens', { model: 'gpt4o', db: flags('hasFullSystemPrompt', 'OAICompletionTokens'), messages: chat })
  add('flag-alternate-role-merge', { model: 'gpt4o', db: flags('hasFullSystemPrompt', 'requiresAlternateRole'), messages: [sys('A'), user('b'), user('c'), bot('d'), bot('e'), user('f')] })
  add('flag-alternate-role-merge-images', { model: 'gpt4o', db: flags('hasFullSystemPrompt', 'requiresAlternateRole'), messages: [user('b'), { role: 'user', content: 'c', multimodals: [image('data:image/png;base64,AAAA')], cachePoint: true }, bot('d')] })
  add('flag-must-start-with-user', { model: 'gpt4o', db: flags('hasFullSystemPrompt', 'mustStartWithUserInput'), messages: [sys('A'), bot('first')] })
  add('flag-first-system-prompt', { model: 'gpt4o', db: flags('hasFirstSystemPrompt'), messages: [sys('A'), sys('B'), user('hi'), sys('C')] })
  add('flag-first-system-prompt-replacement', { model: 'gpt4o', db: { ...flags('hasFirstSystemPrompt'), systemContentReplacement: '[SYSTEM] {{slot}}', systemRoleReplacement: 'assistant' }, messages: [sys('A'), user('hi'), sys('C')] })
  add('flag-first-system-prompt-default-replacement', { model: 'gpt4o', db: flags('hasFirstSystemPrompt'), messages: [sys('A'), user('hi'), sys('C')] })
  add('flag-no-structured-output', { model: 'gpt4o', db: flags('hasFullSystemPrompt', 'noStructuredOutput'), schema: '{"type":"object","properties":{}}', messages: chat })
  add('flag-deepseek-prefix', { model: 'gpt4o', db: flags('hasFullSystemPrompt', 'deepSeekPrefix'), messages: [sys('A'), user('hi'), bot('The reply is')] })
  add('flag-deepseek-thinking-input', { model: 'gpt4o', db: flags('hasFullSystemPrompt', 'deepSeekPrefix', 'deepSeekThinkingInput'), messages: [sys('A'), user('hi'), { role: 'assistant', content: 'Answer', thoughts: ['first', 'second'] }] })
  add('flag-deepseek-thinking-input-user-last', { model: 'gpt4o', db: flags('hasFullSystemPrompt', 'deepSeekPrefix', 'deepSeekThinkingInput'), messages: [sys('A'), { role: 'assistant', content: 'Answer', thoughts: ['first'] }, user('hi')] })
  add('flag-unknown-model-default-id', { model: 'unlisted-model-7b', messages: chat })

  // DeepSeek and DeepInfra catalog models.
  add('deepseek-chat', { model: 'deepseek-chat', messages: [sys('A'), user('hi'), bot('prefix')] })
  add('deepseek-v4-thinking-enabled', { model: 'deepseek-v4-pro', db: { deepseekThinkingType: 'enabled', deepseekReasoningEffort: 'max' }, messages: [sys('A'), user('hi')] })
  add('deepseek-v4-thinking-enabled-default-effort', { model: 'deepseek-v4-flash', db: { deepseekThinkingType: 'enabled' }, messages: [sys('A'), user('hi')] })
  add('deepseek-v4-thinking-disabled', { model: 'deepseek-v4-pro', messages: [sys('A'), user('hi')] })
  add('deepseek-reasoner-no-parameters', { model: 'deepseek-reasoner', messages: [sys('A'), user('hi')] })
  add('deepinfra-model', { model: 'deepinfra_deepseek-ai/DeepSeek-V3', messages: [sys('A'), user('hi'), bot('x')] })
  add('deepseek-missing-key-identifier', { model: 'deepseek-chat', db: { OaiCompAPIKeys: {} }, messages: [sys('A'), user('hi')] })

  // OpenRouter.
  add('openrouter-basic', { model: 'openrouter', messages: chat })
  add('openrouter-stream', { model: 'openrouter', stream: true, messages: chat })
  add('openrouter-fallback-middle-out-provider', { model: 'openrouter', db: { openrouterFallback: true, openrouterMiddleOut: true, openrouterProvider: { order: ['A', 'B'], only: [], ignore: ['C'] } }, messages: chat })
  add('openrouter-empty-provider-lists', { model: 'openrouter', db: { openrouterProvider: { order: [], only: [], ignore: [] } }, messages: chat })
  add('openrouter-bias-removed-for-image-main-model', { model: 'openrouter', mainModel: 'gpt4o', bias: { '5': 1 }, messages: chat })
  add('openrouter-bias-kept-for-text-main-model', { model: 'openrouter', mainModel: 'flag-model', bias: { '5': 1 }, messages: chat })
  add('openrouter-additional-params-all', { model: 'openrouter', db: { applyAdditionalParamsToAll: true, additionalParams: [['provider.sort', '"price"']] }, messages: chat })

  // reverse_proxy.
  const proxy = { forceReplaceUrl: 'https://proxy.example.test/v1/chat/completions', customProxyRequestModel: 'proxy-model', proxyRequestModel: 'custom' }
  add('proxy-basic', { model: 'reverse_proxy', db: proxy, messages: chat })
  add('proxy-stream', { model: 'reverse_proxy', stream: true, db: proxy, messages: chat })
  add('proxy-empty-model-name', { model: 'reverse_proxy', db: { ...proxy, customProxyRequestModel: '' }, messages: chat })
  add('proxy-legacy-model-name', { model: 'reverse_proxy', db: { ...proxy, customProxyRequestModel: 'gpt4o' }, messages: chat })
  add('proxy-autofill-bare-host', { model: 'reverse_proxy', db: { ...proxy, autofillRequestUrl: true, forceReplaceUrl: 'https://proxy.example.test' }, messages: chat })
  add('proxy-autofill-trailing-slash', { model: 'reverse_proxy', db: { ...proxy, autofillRequestUrl: true, forceReplaceUrl: 'https://proxy.example.test/' }, messages: chat })
  add('proxy-autofill-v1', { model: 'reverse_proxy', db: { ...proxy, autofillRequestUrl: true, forceReplaceUrl: 'https://proxy.example.test/v1' }, messages: chat })
  add('proxy-autofill-v1-slash', { model: 'reverse_proxy', db: { ...proxy, autofillRequestUrl: true, forceReplaceUrl: 'https://proxy.example.test/v1/' }, messages: chat })
  add('proxy-autofill-completions-kept', { model: 'reverse_proxy', db: { ...proxy, autofillRequestUrl: true, forceReplaceUrl: 'https://proxy.example.test/api/completions/' }, messages: chat })
  add('proxy-risu-prefix', { model: 'reverse_proxy', db: { ...proxy, forceReplaceUrl: 'risu::https://proxy.example.test/v1/chat/completions' }, messages: chat })
  add('proxy-ooba-mode', {
      model: 'reverse_proxy', db: { ...proxy, reverseProxyOobaMode: true, reverseProxyOobaArgs: { mode: 'chat', top_k: 3, skip: null, nested: { a: [1, 2] }, temperature: 0.1 } },
      messages: [sys('First rule'), user('hi'), sys('Second rule'), bot('hey')],
  })
  add('proxy-flex-official-url', { model: 'reverse_proxy', db: { ...proxy, openAIFlexProcessing: true, forceReplaceUrl: 'https://api.openai.com/v1/chat/completions' }, messages: chat })
  add('proxy-flex-other-url', { model: 'reverse_proxy', db: { ...proxy, openAIFlexProcessing: true }, messages: chat })
  add('proxy-bias-kept-for-gpt-request-model', { model: 'reverse_proxy', mainModel: 'gpt4o', db: { ...proxy, proxyRequestModel: 'gpt4o' }, bias: { '5': 1 }, messages: chat })
  add('proxy-bias-removed-for-other-request-model', { model: 'reverse_proxy', mainModel: 'gpt4o', db: proxy, bias: { '5': 1 }, messages: chat })
  add('proxy-additional-params', { model: 'reverse_proxy', db: { ...proxy, additionalParams: [['top_logprobs', '2'], ['header::X-Org', 'acme']] }, messages: chat })

  // Custom models.
  add('xcustom-basic', { model: 'xcustom:::one', customModels: [customModel()], messages: chat })
  add('xcustom-stream-no-key', { model: 'xcustom:::one', stream: true, customModels: [customModel({ key: undefined })], messages: chat })
  add('xcustom-params', { model: 'xcustom:::one', customModels: [customModel({ params: 'top_logprobs=2\nuser="bob"\nflag=true' })], messages: chat })
  add('xcustom-flex-official-url', { model: 'xcustom:::one', db: { openAIFlexProcessing: true }, customModels: [customModel({ url: 'https://api.openai.com/v1/chat/completions' })], messages: chat })
  add('xcustom-flags', { model: 'xcustom:::one', customModels: [customModel({ flags: [14, 13] })], messages: chat })

  // The URL a buffered request hands to fetch is `new URL(url).href`; a streaming request hands over the string as built.
  const urlCase = (id, forceReplaceUrl, extra = {}) => add(id, { model: 'reverse_proxy', db: { ...proxy, forceReplaceUrl }, messages: chat, ...extra })
  urlCase('proxy-url-uppercase-default-port', 'HTTPS://Proxy.Example.TEST:443/v1/chat/completions')
  urlCase('proxy-url-uppercase-default-port-stream', 'HTTPS://Proxy.Example.TEST:443/v1/chat/completions', { stream: true })
  urlCase('proxy-url-bare-host', 'https://proxy.example.test')
  urlCase('proxy-url-bare-host-stream', 'https://proxy.example.test', { stream: true })
  urlCase('proxy-url-custom-port-leading-zeros', 'http://proxy.example.test:008080/v1/chat/completions')
  urlCase('proxy-url-query-space-fragment', 'https://proxy.example.test:8080/v1/a b?x=1 2&y="q"&z=it#f g')
  urlCase('proxy-url-backslash-and-unicode-path', 'https://proxy.example.test\\v1\\chat/한국{a}')
  urlCase('proxy-url-surrounding-space-and-tab', '  https://proxy.example.test/v1/\tchat/completions  ')
  urlCase('proxy-url-userinfo-declines', 'https://user:pw@proxy.example.test/v1/chat/completions', { declines: 'url-form' })
  urlCase('proxy-url-dot-segment-declines', 'https://proxy.example.test/v1/../v2/chat/completions', { declines: 'url-form' })
  urlCase('proxy-url-numeric-host-declines', 'https://127.1/v1/chat/completions', { declines: 'url-form' })
  urlCase('proxy-url-not-a-url-declines', 'proxy.example.test/v1', { declines: 'url-form' })
  urlCase('proxy-url-not-a-url-stream-keeps-raw', 'proxy.example.test/v1', { stream: true })

  const file = { schema: 1, baseDb, cases }
  writeFileSync(`${out}/requests.cases.json`, JSON.stringify(file, (k, v) => (v === undefined ? undefined : v), 2) + '\n')
    return baseDb
}

function writeResponses(baseDb) {

  const F = { hasFullSystemPrompt: 6, deepSeekThinkingOutput: 19, deepSeekThinkingInput: 18, deepSeekThinkingToggle: 24 }
  const flags = (...names) => ({ enableCustomFlags: true, customFlags: names.map(name => F[name]) })
  const chat = [{ role: 'system', content: 'Be brief.' }, { role: 'user', content: 'Hello' }]
  const ok = json => ({ status: 200, json })
  const msg = (content, extra = {}) => ({ role: 'assistant', content, ...extra })
  const one = (message, extra = {}) => ok({ choices: [{ index: 0, message, finish_reason: 'stop', ...extra }], usage: { prompt_tokens: 3, completion_tokens: 2 } })
  const think = flags('hasFullSystemPrompt', 'deepSeekThinkingOutput')
  const fn = (id, name, args) => ({ id, type: 'function', function: { name, arguments: args } })
  const lookup = { name: 'lookup', description: 'Find a record', inputSchema: { type: 'object', properties: { query: { type: 'string' } }, required: ['query'] } }
  const other = { name: 'other', description: 'Another', inputSchema: { type: 'object' } }
  const text = value => [{ type: 'text', text: value }]
  const schema = '{"type":"object","properties":{"answer":{"type":"string"}}}'

  const cases = []
  const add = (id, entry) => cases.push({ id, model: 'gpt4o', messages: chat, ...entry })

  // Plain content.
  add('content-basic', { reply: one(msg('Hi there.')) })
  add('content-empty', { reply: one(msg('')) })
  add('content-null', { reply: one(msg(null)) })
  add('content-missing', { reply: ok({ choices: [{ message: { role: 'assistant' } }] }) })
  add('content-unicode', { reply: one(msg('안녕하세요 \u{1F600} "q" \\ \n tab\t')) })
  add('legacy-text-field', { reply: ok({ choices: [{ text: 'completion text' }] }) })
  add('legacy-empty-text-falls-back-to-message', { reply: ok({ choices: [{ text: '', message: msg('from message') }] }) })
  add('legacy-text-extract-json-string-value', { extractJson: 'answer', schema, reply: ok({ choices: [{ text: JSON.stringify('{"answer":"yes"}') }] }) })
  add('legacy-text-extract-json-invalid', { extractJson: 'answer', schema, reply: ok({ choices: [{ text: 'not json' }] }) })

  // Reasoning.
  add('reasoning-content-in-message', { reply: one(msg('Answer', { reasoning_content: 'I thought.' })) })
  add('reasoning-content-in-choice', { reply: ok({ choices: [{ message: msg('Answer'), reasoning_content: 'Choice level.' }] }) })
  add('reasoning-content-skipped-when-already-thoughts', { reply: one(msg('<Thoughts>\nx\n</Thoughts>\nAnswer', { reasoning_content: 'ignored' })) })
  add('reasoning-openrouter-field', { reply: one(msg('Answer', { reasoning: 'Router reasoning.' })) })
  add('reasoning-both-fields', { reply: one(msg('Answer', { reasoning_content: 'First.', reasoning: 'Second.' })) })
  add('reasoning-empty-field-ignored', { reply: one(msg('Answer', { reasoning_content: '' })) })
  add('think-tags-split', { db: think, model: 'gpt4o', reply: one(msg('<think>Hmm.</think>Answer')) })
  add('think-close-only', { db: think, model: 'gpt4o', reply: one(msg('Hmm. so.</think>Answer')) })
  add('think-no-close-tag', { db: think, model: 'gpt4o', reply: one(msg('<think>unfinished Answer')) })
  add('think-skipped-when-reasoning-field-present', { db: think, model: 'gpt4o', reply: one(msg('<think>x</think>Answer', { reasoning_content: 'real' })) })
  add('think-last-close-tag-wins', { db: think, model: 'gpt4o', reply: one(msg('a</think>b</think>c')) })
  add('think-multiline', { db: think, model: 'gpt4o', reply: one(msg('<think>\nline1\nline2\n</think>\nAnswer\n')) })
  add('think-empty-reasoning', { db: think, model: 'gpt4o', reply: one(msg('</think>Answer')) })
  add('think-ignored-without-flag', { reply: one(msg('<think>x</think>Answer')) })

  // Extract JSON.
  const extract = (id, format, content, extra = {}) => add(id, { extractJson: format, schema, reply: one(msg(content)), ...extra })
  extract('extract-string-value', 'answer', '{"answer":"yes"}')
  extract('extract-number-value', 'n', '{"n":42.5}')
  extract('extract-boolean-value', 'ok', '{"ok":true}')
  extract('extract-null-value', 'v', '{"v":null}')
  extract('extract-missing-key', 'zzz', '{"answer":"yes"}')
  extract('extract-not-json-object', 'answer', '  plain words  ')
  extract('extract-code-fence', 'answer', '```json\n{"answer":"fenced"}\n```')
  extract('extract-thoughts-removed', 'answer', '<Thoughts>\nhidden\n</Thoughts>\n{"answer":"a"}')
  extract('extract-invalid-json', 'answer', '{"answer": }')
  extract('extract-dotted-reaches-object', 'a.b', '{"a":{"b":"deep"}}', { declines: 'extract-json-nested-object' })
  extract('extract-dotted-scalar', 'a.b', '{"a":"flat"}')
  extract('extract-database-schema', 'answer', '{"answer":"db"}', { schema: undefined, db: { jsonSchemaEnabled: true } })
  add('extract-ignored-without-schema', { extractJson: 'answer', reply: one(msg('{"answer":"raw"}')) })
  add('extract-multiline', { db: { genTime: 2 }, extractJson: 'answer', schema, reply: ok({ choices: [{ message: msg('{"answer":"one"}') }, { message: msg('{"answer":"two"}') }] }) })

  // Multiline.
  add('multiline-two-choices', { db: { genTime: 2 }, reply: ok({ choices: [{ message: msg('one') }, { message: msg('two') }] }) })
  add('multiline-null-content', { db: { genTime: 2 }, reply: ok({ choices: [{ message: msg(null) }, { message: msg('two') }] }) })
  add('multiline-continue-is-single', { db: { genTime: 2 }, continue: true, reply: ok({ choices: [{ message: msg('one') }, { message: msg('two') }] }) })

  // Errors.
  add('error-message', { reply: { status: 429, json: { error: { message: 'Rate limit reached', type: 'rate' } } } })
  add('error-without-message', { reply: { status: 500, json: { error: { type: 'server' } } } })
  add('error-without-error-key', { reply: { status: 503, json: { detail: 'unavailable' } } })
  add('error-empty-message', { reply: { status: 400, json: { error: { message: '' } } } })
  add('error-message-not-text', { reply: { status: 400, json: { error: { message: 7 } } } })
  add('ok-without-choices', { reply: ok({ result: 'nothing' }) })
  add('ok-empty-choices', { reply: ok({ choices: [] }) })
  add('ok-null-message', { reply: ok({ choices: [{ message: null }] }) })

  // Tool calls.
  const calls = (...items) => msg(null, { tool_calls: items })
  add('tool-single-call', {
      tools: [lookup], replies: [one(calls(fn('call_1', 'lookup', '{"query":"cats"}'))), one(msg('Cats are mammals.'))], toolReplies: [text('cat facts')],
  })
  add('tool-call-with-text', {
      tools: [lookup], replies: [one(msg('Let me look.', { tool_calls: [fn('call_1', 'lookup', '{"query":"x"}')] })), one(msg('Done.'))], toolReplies: [text('result')],
  })
  add('tool-unknown-tool', { tools: [lookup], replies: [one(calls(fn('call_1', 'missing', '{}'))), one(msg('Sorry.'))] })
  add('tool-no-text-response', { tools: [lookup], replies: [one(calls(fn('call_1', 'lookup', '{"query":"x"}'))), one(msg('Empty.'))], toolReplies: [[{ type: 'image', text: 'x' }]] })
  add('tool-throws', { tools: [lookup], replies: [one(calls(fn('call_1', 'lookup', '{"query":"x"}'))), one(msg('Failed.'))], toolReplies: [{ throws: 'boom' }] })
  add('tool-first-text-entry-wins', { tools: [lookup], replies: [one(calls(fn('call_1', 'lookup', '{"query":"x"}'))), one(msg('ok'))], toolReplies: [[{ type: 'image' }, { type: 'text', text: 'first' }, { type: 'text', text: 'second' }]] })
  add('tool-simplified-tool-use', {
      db: { simplifiedToolUse: true }, tools: [lookup], replies: [one(msg('Thinking aloud.', { tool_calls: [fn('call_1', 'lookup', '{"query":"x"}')] })), one(msg('Final.'))], toolReplies: [text('r')],
  })
  add('tool-two-calls', {
      tools: [lookup, other], replies: [one(calls(fn('call_1', 'lookup', '{"query":"a"}'), fn('call_2', 'other', '{}'))), one(msg('Both done.'))], toolReplies: [text('A'), text('B')],
  })
  add('tool-calls-across-choices', {
      tools: [lookup], replies: [ok({ choices: [{ message: calls(fn('call_1', 'lookup', '{"query":"a"}')) }, { message: calls(fn('call_2', 'lookup', '{"query":"b"}')) }] }), one(msg('Merged.'))],
      toolReplies: [text('A'), text('B')],
  })
  add('tool-empty-arguments', { tools: [lookup], replies: [one(calls(fn('call_1', 'lookup', ''))), one(msg('Empty args.'))], toolReplies: [text('r')] })
  add('tool-call-without-tools-sent', { replies: [one(calls(fn('call_1', 'lookup', '{"query":"x"}'))), one(msg('No tools.'))] })
  add('tool-call-without-name-skipped', { tools: [lookup], replies: [one(calls({ id: 'call_1', type: 'function', function: { arguments: '{}' } })), one(msg('Skipped.'))] })
  add('tool-assistant-keeps-provider-fields', {
      tools: [lookup], replies: [one(msg('hm', { reasoning_content: 'because', refusal: null, tool_calls: [fn('call_1', 'lookup', '{"query":"x"}')] })), one(msg('Fine.'))], toolReplies: [text('r')],
  })
  add('tool-follow-up-fails', { tools: [lookup], replies: [one(calls(fn('call_1', 'lookup', '{"query":"x"}'))), { status: 500, json: { error: { message: 'down' } } }], toolReplies: [text('r')] })
  add('tool-follow-up-asks-again', {
      tools: [lookup],
      replies: [one(calls(fn('call_1', 'lookup', '{"query":"a"}'))), one(calls(fn('call_2', 'lookup', '{"query":"b"}'))), one(msg('Third.'))],
      toolReplies: [text('A'), text('B')],
  })
  add('tool-deepseek-follow-up-body', {
      db: { ...flags('hasFullSystemPrompt', 'deepSeekThinkingInput'), deepseekThinkingType: 'enabled' }, model: 'gpt4o', tools: [lookup],
      replies: [one(msg('x', { reasoning_content: 'why', tool_calls: [fn('call_1', 'lookup', '{"query":"x"}')] })), one(msg('Fine.'))], toolReplies: [text('r')],
  })
  add('tool-thoughts-and-text-prefix', {
      db: think, model: 'gpt4o', tools: [lookup],
      replies: [one(msg('<think>pondering</think>Visible.', { tool_calls: [fn('call_1', 'lookup', '{"query":"x"}')] })), one(msg('End.'))], toolReplies: [text('r')],
  })

  // Bodies that are not JSON: `response.json()` throws and `globalFetch` answers `{ ok: false, data: "SyntaxError: ...", status: 400 }`.
  add('text-body-not-json-200', { reply: { status: 200, text: 'not json' } })
  add('text-body-html-error-status', { reply: { status: 502, text: '<html><body>Bad gateway page</body></html>' } })
  add('text-body-empty', { reply: { status: 200, text: '' } })
  add('text-body-whitespace-only', { reply: { status: 200, text: ' \n ' } })
  add('text-body-plain-words', { reply: { status: 503, text: 'Service Unavailable' } })
  add('text-body-long-garbage', { reply: { status: 200, text: 'abcdefghijklmnopqrstuvwxyz' } })
  add('text-body-leading-whitespace', { reply: { status: 200, text: '   hello' } })
  add('text-body-boundary-length-21', { reply: { status: 200, text: 'x23456789012345678901' } })
  add('text-body-bom-then-garbage', { reply: { status: 200, text: '﻿abc' } })
  add('text-body-korean-garbage', { reply: { status: 200, text: '안녕하세요 서버 오류입니다 잠시 후 다시 시도해 주세요' } })
  add('text-body-truncated-json', { reply: { status: 200, text: '{"choices":[{"message":{"content":"cut' }, declines: 'json-parse-message' })
  add('text-body-unmodelled-message', { reply: { status: 200, text: '{"a" 1}' }, declines: 'json-parse-message' })
  add('transport-error', { reply: { throws: 'fetch failed' } })

  add('error-message-over-the-limit', { reply: { status: 500, json: { error: { message: `${'c'.repeat(2047)}\u{1F600}tail` } } } })
  add('error-message-padded', { reply: { status: 400, json: { error: { message: '  spaced out  ' } } } })

  // reverse_proxy, openrouter and custom models.
  const proxyDb = { forceReplaceUrl: 'https://proxy.example.test/v1/chat/completions', customProxyRequestModel: 'proxy-model', proxyRequestModel: 'custom' }
  const customOne = { id: 'xcustom:::one', name: 'One', internalId: 'my-model', url: 'https://llm.example.test/v1/chat/completions', key: 'sk-synthetic-custom', format: 0, flags: [], tokenizer: 0, params: '' }
  add('proxy-content', { model: 'reverse_proxy', db: proxyDb, reply: one(msg('Proxy says hi.')) })
  add('proxy-error-body', { model: 'reverse_proxy', db: proxyDb, reply: { status: 401, json: { error: { message: 'bad key' } } } })
  add('proxy-tool-round', { model: 'reverse_proxy', db: proxyDb, tools: [lookup], replies: [one(calls(fn('call_1', 'lookup', '{"query":"x"}'))), one(msg('Done.'))], toolReplies: [text('r')] })
  add('proxy-multi-gen-not-gpt', { model: 'reverse_proxy', db: { ...proxyDb, genTime: 2 }, reply: one(msg('Single.')) })
  add('openrouter-content-with-reasoning', { model: 'openrouter', reply: one(msg('Answer', { reasoning: 'Router reasoning.' })) })
  add('openrouter-error-body', { model: 'openrouter', reply: { status: 402, json: { error: { message: 'Insufficient credits' } } } })
  add('openrouter-tool-round', { model: 'openrouter', tools: [lookup], replies: [one(calls(fn('call_1', 'lookup', '{"query":"x"}'))), one(msg('Done.'))], toolReplies: [text('r')] })
  add('xcustom-content', { model: 'xcustom:::one', customModels: [customOne], reply: one(msg('Custom says hi.')) })
  add('xcustom-without-key', { model: 'xcustom:::one', customModels: [{ ...customOne, key: undefined }], reply: one(msg('No key.')) })
  add('gpt-multi-gen', { db: { genTime: 2 }, reply: ok({ choices: [{ index: 0, message: msg('One') }, { index: 1, message: msg('Two') }] }) })

  // rememberToolUsage appends the encoded call to the text.
  add('remember-tool-usage', { rememberToolUsage: true, tools: [lookup], replies: [one(msg('Looking. ', { tool_calls: [fn('call_1', 'lookup', '{"query":"a"}')] })), one(msg('Found.'))], toolReplies: [text('A')] })
  add('remember-tool-usage-two-calls', {
      rememberToolUsage: true, tools: [lookup, other],
      replies: [one(calls(fn('call_1', 'lookup', '{"query":"a"}'), fn('call_2', 'other', '{}'))), one(msg('Both.'))], toolReplies: [text('A'), text('B')],
  })
  add('remember-tool-usage-simplified', {
      rememberToolUsage: true, db: { simplifiedToolUse: true }, tools: [lookup],
      replies: [one(msg('Hidden. ', { tool_calls: [fn('call_1', 'lookup', '{"query":"a"}')] })), one(msg('Found.'))], toolReplies: [text('A')],
  })
  add('remember-tool-usage-follow-up-fails', {
      rememberToolUsage: true, tools: [lookup],
      replies: [one(msg('Before. ', { tool_calls: [fn('call_1', 'lookup', '{"query":"a"}')] })), { status: 500, json: { error: { message: 'down' } } }], toolReplies: [text('A')],
  })
  add('remember-tool-usage-no-text-response-leaves-no-code', {
      rememberToolUsage: true, tools: [lookup], replies: [one(calls(fn('call_1', 'lookup', '{"query":"a"}'))), one(msg('Done.'))], toolReplies: [[{ type: 'image' }]],
  })
  add('remember-tool-usage-unknown-tool-leaves-no-code', {
      rememberToolUsage: true, tools: [lookup], replies: [one(calls(fn('call_1', 'missing', '{}'))), one(msg('Done.'))],
  })

  // Tool call ids and names are forwarded as the model sent them.
  add('tool-numeric-id', { tools: [lookup], replies: [one(calls(fn(7, 'lookup', '{"query":"x"}'))), one(msg('Done.'))], toolReplies: [text('r')] })
  add('tool-null-id', { tools: [lookup], replies: [one(calls(fn(null, 'lookup', '{"query":"x"}'))), one(msg('Done.'))], toolReplies: [text('r')] })
  add('tool-missing-id', { tools: [lookup], replies: [one(calls(fn(undefined, 'lookup', '{"query":"x"}'))), one(msg('Done.'))], toolReplies: [text('r')] })
  add('tool-numeric-name', { tools: [lookup], replies: [one(calls(fn('call_1', 5, '{}'))), one(msg('Done.'))] })
  add('tool-boolean-name', { tools: [lookup], replies: [one(calls(fn('call_1', true, '{}'))), one(msg('Done.'))] })
  add('tool-array-name', { tools: [lookup], replies: [one(calls(fn('call_1', ['a', 'b'], '{}'))), one(msg('Done.'))] })
  add('tool-object-id', { tools: [lookup], replies: [one(calls(fn({ n: 1 }, 'lookup', '{"query":"x"}'))), one(msg('Done.'))], toolReplies: [text('r')] })
  add('tool-arguments-invalid-json', { tools: [lookup], replies: [one(calls(fn('call_1', 'lookup', 'oops'))), one(msg('Done.'))] })
  add('tool-arguments-invalid-json-without-tools', { replies: [one(calls(fn('call_1', 'lookup', 'oops'))), one(msg('Done.'))] })
  add('tool-arguments-not-text-declines', { tools: [lookup], replies: [one(calls(fn('call_1', 'lookup', 5))), one(msg('Done.'))], toolReplies: [text('r')], declines: 'tool-call-arguments-not-text' })
  add('tool-second-response-tool-calls-string-declines-after-tools-ran', {
      tools: [lookup], replies: [one(calls(fn('call_1', 'lookup', '{"query":"x"}'))), one(msg('Again.', { tool_calls: 'oops' }))], toolReplies: [text('r')],
      declines: 'tool-calls-not-array', terminal: true,
  })
  add('tool-calls-string-declines-before-tools', { tools: [lookup], replies: [one(msg('Hm.', { tool_calls: 'oops' }))], declines: 'tool-calls-not-array' })

  writeFileSync(`${out}/responses.cases.json`, JSON.stringify({ schema: 1, baseDb, cases }, null, 2) + '\n')
}

function writeStreams(baseDb) {

  const F = { hasFullSystemPrompt: 6, deepSeekThinkingOutput: 19, deepSeekThinkingInput: 18 }
  const flags = (...names) => ({ enableCustomFlags: true, customFlags: names.map(name => F[name]) })
  const chat = [{ role: 'system', content: 'Be brief.' }, { role: 'user', content: 'Hello' }]
  const think = flags('hasFullSystemPrompt', 'deepSeekThinkingOutput')
  const lookup = { name: 'lookup', description: 'Find a record', inputSchema: { type: 'object', properties: { query: { type: 'string' } }, required: ['query'] } }
  const other = { name: 'other', description: 'Another', inputSchema: { type: 'object' } }
  const text = value => [{ type: 'text', text: value }]
  const schema = '{"type":"object","properties":{"answer":{"type":"string"}}}'

  const ev = value => `data: ${JSON.stringify(value)}\n\n`
  const delta = (content, extra = {}) => ev({ choices: [{ index: 0, delta: { content }, ...extra }] })
  const reasoning = (value, field = 'reasoning_content') => ev({ choices: [{ index: 0, delta: { [field]: value } }] })
  const call = (index, fields) => ev({ choices: [{ index: 0, delta: { tool_calls: [{ index, ...fields }] } }] })
  const done = 'data: [DONE]\n\n'
  const stream = (...parts) => ({ chunks: parts.map(part => (typeof part === 'string' ? { utf8: part } : part)) })
  const whole = (...parts) => stream(parts.join(''))
  const bytes = (value, size) => {
      const data = Buffer.from(value, 'utf8')
      const out = []
      for (let index = 0; index < data.length; index += size) out.push({ hex: data.subarray(index, index + size).toString('hex') })
      return { chunks: out }
  }

  const cases = []
  const add = (id, entry) => cases.push({ id, model: 'gpt4o', stream: true, messages: chat, ...entry })

  // Text.
  add('text-two-events', { reply: stream(delta('Hel'), delta('lo'), done) })
  add('text-all-in-one-chunk', { reply: whole(delta('Hel'), delta('lo'), done) })
  add('text-no-done-marker', { reply: stream(delta('Hel'), delta('lo')) })
  add('text-split-inside-line', { reply: stream('data: {"choices":[{"index":0,"del', 'ta":{"content":"Hi"}}]}\n', '\ndata: [DO', 'NE]\n\n') })
  add('text-split-every-byte', { reply: bytes(delta('Hi there') + delta('!') + done, 1) })
  add('text-multibyte-split-across-chunks', { reply: bytes(delta('안녕') + delta('하세요 \u{1F600}') + done, 5) })
  add('text-crlf-line-endings', { reply: stream(delta('Hi').replace(/\n/g, '\r\n'), done.replace(/\n/g, '\r\n')) })
  add('text-blank-lines-and-comments', { reply: stream(': keep-alive\n\n', 'event: message\n', delta('Hi'), '\n\n', done) })
  add('text-malformed-line-ignored', { reply: stream(delta('A'), 'data: {not json}\n\n', delta('B'), done) })
  add('text-line-without-choices-ignored', { reply: stream(delta('A'), ev({ id: 'x' }), delta('B'), done) })
  add('text-empty-choices-usage-chunk', { reply: stream(delta('A'), ev({ choices: [], usage: { total_tokens: 3 } }), done) })
  add('text-null-and-empty-content-skipped', { reply: stream(delta(null), delta(''), delta('A'), done) })
  add('text-role-only-first-chunk', { reply: stream(ev({ choices: [{ index: 0, delta: { role: 'assistant' } }] }), delta('A'), done) })
  add('text-completion-style-text-field', { reply: stream(ev({ choices: [{ index: 0, text: 'old ' }] }), ev({ choices: [{ index: 0, text: 'style' }] }), done) })
  add('text-null-content-uses-text-field', { reply: stream(ev({ choices: [{ index: 0, delta: { content: null }, text: 'fallback' }] }), done) })
  add('text-cumulative-fragments-replace', { reply: stream(delta('Hel'), delta('Hello'), delta('Hello world'), done) })
  add('text-repeated-fragment-appends', { reply: stream(delta('a'), delta('a'), delta('a'), done) })
  add('text-fragment-longer-but-different', { reply: stream(delta('abc'), delta('xyzw'), done) })
  add('text-lines-after-done-ignored', { reply: stream(delta('A'), done, delta('B'), done) })
  add('text-empty-body', { reply: stream() })
  add('text-only-done', { reply: stream(done) })
  add('text-unicode-escapes-and-quotes', { reply: stream(delta('He said "hi"\n\t\\ é'), done) })
  add('text-choice-delta-missing-throws', { reply: stream(delta('A'), ev({ choices: [{ index: 0 }] }), delta('B'), done) })

  // Reasoning.
  add('reasoning-content-then-text', { reply: stream(reasoning('Think '), reasoning('hard.'), delta('Answer'), done) })
  add('reasoning-openrouter-field', { reply: stream(reasoning('Router.', 'reasoning'), delta('Answer'), done) })
  add('reasoning-only', { reply: stream(reasoning('Only thoughts'), done) })
  add('reasoning-cumulative-fragments', { reply: stream(reasoning('ab'), reasoning('abc'), delta('X'), done) })
  add('reasoning-then-no-done', { reply: stream(reasoning('Think'), delta('Answer')) })
  add('think-tags-in-content', { db: think, model: 'gpt4o', reply: stream(delta('<think>Hmm'), delta('.</think>'), delta('Answer'), done) })
  add('think-tags-without-done-marker', { db: think, model: 'gpt4o', reply: stream(delta('<think>Hmm.</think>Answer')) })
  add('think-open-tag-only', { db: think, model: 'gpt4o', reply: stream(delta('<think>still going'), done) })
  add('think-structured-reasoning-disables-split', { db: think, model: 'gpt4o', reply: stream(reasoning('real'), delta('<think>x</think>Answer'), done) })
  add('think-flag-without-any-content', { db: think, model: 'gpt4o', reply: stream(done) })
  add('think-flag-without-content-before-chunks', { db: think, model: 'gpt4o', reply: stream(ev({ id: 'x' }), delta('A'), done) })

  // Extract JSON and multigen.
  add('extract-json-stream', { extractJson: 'answer', schema, reply: stream(delta('{"answer":'), delta('"yes"}'), done) })
  add('extract-json-partial-is-raw', { extractJson: 'answer', schema, reply: stream(delta('{"answ'), delta('er":"yes"}')) })
  add('multigen-stream-indexes', { db: { genTime: 2 }, reply: stream(ev({ choices: [{ index: 0, delta: { content: 'A' } }, { index: 1, delta: { content: 'B' } }] }), delta('a'), done) })
  add('multigen-stream-missing-index-ignored', { db: { genTime: 2 }, reply: stream(ev({ choices: [{ delta: { content: 'A' } }] }), ev({ choices: [{ index: 1, delta: { content: 'B' } }] }), done) })
  add('multigen-stream-only-second-index', { db: { genTime: 2 }, reply: stream(ev({ choices: [{ index: 1, delta: { content: 'B' } }] }), done) })

  // Start of the stream.
  add('start-error-status', { reply: { status: 429, contentType: 'application/json', chunks: [{ utf8: '{"error":{"message":"slow down"}}' }] } })
  add('start-wrong-content-type', { reply: { status: 200, contentType: 'application/json', chunks: [{ utf8: '{"choices":[]}' }] } })
  add('start-event-stream-with-charset', { reply: { status: 200, contentType: 'text/event-stream; charset=utf-8', chunks: [{ utf8: delta('ok') + done }] } })
  add('start-non-200-event-stream', { reply: { status: 201, contentType: 'text/event-stream', chunks: [{ utf8: delta('ok') + done }] } })

  // Tool rounds.
  const toolRound = (id, fields = {}) => ({ db: {}, tools: [lookup], ...fields, id })
  const addTool = (id, entry) => add(id, entry)
  addTool('tool-single-call', {
      tools: [lookup],
      replies: [stream(call(0, { id: 'call_1', function: { name: 'lookup', arguments: '' } }), call(0, { function: { arguments: '{"query":' } }), call(0, { function: { arguments: '"cats"}' } }), done), stream(delta('Cats are mammals.'), done)],
      toolReplies: [text('cat facts')],
  })
  addTool('tool-call-with-leading-text', {
      tools: [lookup],
      replies: [stream(delta('Let me look. '), call(0, { id: 'call_1', function: { name: 'lookup', arguments: '{"query":"x"}' } }), done), stream(delta('Done.'), done)],
      toolReplies: [text('result')],
  })
  addTool('tool-call-without-index', {
      tools: [lookup],
      replies: [stream(call(undefined, { id: 'call_1', function: { name: 'lookup', arguments: '{"query":"x"}' } }), done), stream(delta('Ok.'), done)],
      toolReplies: [text('r')],
  })
  addTool('tool-two-parallel-calls', {
      tools: [lookup, other],
      replies: [
          stream(call(0, { id: 'call_1', function: { name: 'lookup', arguments: '{"query":"a"}' } }), call(1, { id: 'call_2', function: { name: 'other', arguments: '{}' } }), done),
          stream(delta('Both.'), done),
      ],
      toolReplies: [text('A'), text('B')],
  })
  addTool('tool-calls-arrive-interleaved', {
      tools: [lookup, other],
      replies: [
          stream(call(1, { id: 'call_2', function: { name: 'other' } }), call(0, { id: 'call_1', function: { name: 'lookup' } }), call(0, { function: { arguments: '{"query":"a"}' } }), call(1, { function: { arguments: '{}' } }), done),
          stream(delta('Interleaved.'), done),
      ],
      toolReplies: [text('A'), text('B')],
  })
  addTool('tool-arguments-cumulative-fragments', {
      tools: [lookup],
      replies: [stream(call(0, { id: 'call_1', function: { name: 'lookup', arguments: '{"query' } }), call(0, { function: { arguments: '{"query":"x"}' } }), done), stream(delta('Replaced.'), done)],
      toolReplies: [text('r')],
  })
  addTool('tool-id-and-name-arrive-late', {
      tools: [lookup],
      replies: [stream(call(0, { function: { arguments: '{"query":"x"}' } }), call(0, { id: 'call_late' }), call(0, { function: { name: 'lookup' } }), done), stream(delta('Late.'), done)],
      toolReplies: [text('r')],
  })
  addTool('tool-unknown-tool', { tools: [lookup], replies: [stream(call(0, { id: 'call_1', function: { name: 'missing', arguments: '{}' } }), done), stream(delta('Sorry.'), done)] })
  addTool('tool-throws', { tools: [lookup], replies: [stream(call(0, { id: 'call_1', function: { name: 'lookup', arguments: '{"query":"x"}' } }), done), stream(delta('Failed.'), done)], toolReplies: [{ throws: 'boom' }] })
  addTool('tool-no-text-response', { tools: [lookup], replies: [stream(call(0, { id: 'call_1', function: { name: 'lookup', arguments: '{"query":"x"}' } }), done), stream(delta('Empty.'), done)], toolReplies: [[{ type: 'image' }]] })
  addTool('tool-empty-arguments-skipped', { tools: [lookup], replies: [stream(call(0, { id: 'call_1', function: { name: 'lookup' } }), done), stream(delta('Skipped.'), done)] })
  addTool('tool-call-without-name-skipped', { tools: [lookup], replies: [stream(call(0, { id: 'call_1', function: { arguments: '{}' } }), done), stream(delta('Skipped.'), done)] })
  addTool('tool-simplified-tool-use', {
      db: { simplifiedToolUse: true }, tools: [lookup],
      replies: [stream(delta('Thinking aloud. '), call(0, { id: 'call_1', function: { name: 'lookup', arguments: '{"query":"x"}' } }), done), stream(delta('Final.'), done)],
      toolReplies: [text('r')],
  })
  addTool('tool-tools-not-sent', {
      replies: [stream(call(0, { id: 'call_1', function: { name: 'lookup', arguments: '{"query":"x"}' } }), done), stream(delta('No tools.'), done)],
  })
  addTool('tool-second-round', {
      tools: [lookup],
      replies: [
          stream(call(0, { id: 'call_1', function: { name: 'lookup', arguments: '{"query":"a"}' } }), done),
          stream(delta('Again. '), call(0, { id: 'call_2', function: { name: 'lookup', arguments: '{"query":"b"}' } }), done),
          stream(delta('Third.'), done),
      ],
      toolReplies: [text('A'), text('B')],
  })
  addTool('tool-follow-up-error-status', {
      tools: [lookup],
      replies: [stream(delta('Before. '), call(0, { id: 'call_1', function: { name: 'lookup', arguments: '{"query":"x"}' } }), done), { status: 500, contentType: 'application/json', chunks: [{ utf8: '{}' }] }],
      toolReplies: [text('r')],
  })
  addTool('tool-follow-up-wrong-content-type', {
      tools: [lookup],
      replies: [stream(call(0, { id: 'call_1', function: { name: 'lookup', arguments: '{"query":"x"}' } }), done), { status: 200, contentType: 'text/plain', chunks: [{ utf8: 'nope' }] }],
      toolReplies: [text('r')],
  })
  addTool('tool-reasoning-passes-to-assistant-message', {
      db: flags('hasFullSystemPrompt', 'deepSeekThinkingInput'), tools: [lookup],
      replies: [stream(reasoning('why'), delta('Visible. '), call(0, { id: 'call_1', function: { name: 'lookup', arguments: '{"query":"x"}' } }), done), stream(delta('Fine.'), done)],
      toolReplies: [text('r')],
  })
  addTool('tool-reasoning-dropped-without-flag', {
      tools: [lookup],
      replies: [stream(reasoning('why'), delta('Visible. '), call(0, { id: 'call_1', function: { name: 'lookup', arguments: '{"query":"x"}' } }), done), stream(delta('Fine.'), done)],
      toolReplies: [text('r')],
  })
  addTool('tool-calls-then-follow-up-prefix-with-thoughts', {
      db: think, tools: [lookup],
      replies: [stream(delta('<think>pondering</think>Visible.'), call(0, { id: 'call_1', function: { name: 'lookup', arguments: '{"query":"x"}' } }), done), stream(delta('End.'), done)],
      toolReplies: [text('r')],
  })
  addTool('tool-call-split-across-bytes', {
      tools: [lookup],
      replies: [bytes(call(0, { id: 'call_1', function: { name: 'lookup', arguments: '{"query":"한국"}' } }) + done, 3), stream(delta('Ok.'), done)],
      toolReplies: [text('r')],
  })

  // Error bodies are read as `Response.text()`: a byte order mark is removed and invalid sequences are replaced.
  const hex = value => Buffer.from(value, 'utf8').toString('hex')
  add('start-error-body-bom', { reply: { status: 500, contentType: 'text/plain', chunks: [{ hex: `efbbbf${hex('upstream down')}` }] } })
  add('start-error-body-invalid-utf8', { reply: { status: 502, contentType: 'text/plain', chunks: [{ hex: '6162ff63' }, { hex: 'e2' }] } })
  add('start-error-body-multibyte-split', { reply: { status: 500, contentType: 'text/plain', chunks: [{ hex: hex('안녕').slice(0, 8) }, { hex: hex('안녕').slice(8) }] } })
  add('start-error-body-empty', { reply: { status: 500, contentType: 'text/plain', chunks: [] } })
  add('start-error-body-padded', { reply: { status: 500, contentType: 'text/plain', chunks: [{ utf8: '  \n gateway down \n' }] } })
  // A failure keeps the first 2048 UTF-16 units of the body; the cut never splits a surrogate pair.
  add('start-error-body-over-the-message-limit', { reply: { status: 500, contentType: 'text/plain', chunks: [{ utf8: `${'a'.repeat(2047)}\u{1F600}tail and more` }] } })
  add('start-error-body-at-the-message-limit', { reply: { status: 500, contentType: 'text/plain', chunks: [{ utf8: `  ${'b'.repeat(2048)}  ` }] } })
  add('start-200-wrong-content-type-body-bom', { reply: { status: 200, contentType: 'text/html', chunks: [{ hex: `efbbbf${hex('<html>')}` }] } })

  // reverse_proxy and openrouter.
  const proxyDb = { forceReplaceUrl: 'https://proxy.example.test/v1/chat/completions', customProxyRequestModel: 'proxy-model', proxyRequestModel: 'custom' }
  add('proxy-text', { model: 'reverse_proxy', db: proxyDb, reply: stream(delta('Proxy '), delta('says hi.'), done) })
  add('proxy-error-status', { model: 'reverse_proxy', db: proxyDb, reply: { status: 401, contentType: 'application/json', chunks: [{ utf8: '{"error":{"message":"bad key"}}' }] } })
  add('proxy-tool-round', {
      model: 'reverse_proxy', db: proxyDb, tools: [lookup],
      replies: [stream(call(0, { id: 'call_1', function: { name: 'lookup', arguments: '{"query":"x"}' } }), done), stream(delta('Done.'), done)], toolReplies: [text('r')],
  })
  add('openrouter-text-with-reasoning', { model: 'openrouter', reply: stream(reasoning('Router.', 'reasoning'), delta('Answer'), done) })
  add('openrouter-tool-round', {
      model: 'openrouter', tools: [lookup],
      replies: [stream(call(0, { id: 'call_1', function: { name: 'lookup', arguments: '{"query":"x"}' } }), done), stream(delta('Done.'), done)], toolReplies: [text('r')],
  })

  // rememberToolUsage.
  add('remember-tool-usage', {
      rememberToolUsage: true, tools: [lookup],
      replies: [stream(delta('Looking. '), call(0, { id: 'call_1', function: { name: 'lookup', arguments: '{"query":"a"}' } }), done), stream(delta('Found.'), done)], toolReplies: [text('A')],
  })
  add('remember-tool-usage-two-calls', {
      rememberToolUsage: true, tools: [lookup, other],
      replies: [stream(call(0, { id: 'call_1', function: { name: 'lookup', arguments: '{"query":"a"}' } }), call(1, { id: 'call_2', function: { name: 'other', arguments: '{}' } }), done), stream(delta('Both.'), done)],
      toolReplies: [text('A'), text('B')],
  })
  add('remember-tool-usage-simplified', {
      rememberToolUsage: true, db: { simplifiedToolUse: true }, tools: [lookup],
      replies: [stream(delta('Hidden. '), call(0, { id: 'call_1', function: { name: 'lookup', arguments: '{"query":"a"}' } }), done), stream(delta('Found.'), done)], toolReplies: [text('A')],
  })
  add('remember-tool-usage-second-round', {
      rememberToolUsage: true, tools: [lookup],
      replies: [
          stream(delta('One. '), call(0, { id: 'call_1', function: { name: 'lookup', arguments: '{"query":"a"}' } }), done),
          stream(delta('Two. '), call(0, { id: 'call_2', function: { name: 'lookup', arguments: '{"query":"b"}' } }), done),
          stream(delta('Three.'), done),
      ],
      toolReplies: [text('A'), text('B')],
  })

  // Tool call ids and names are forwarded as the model sent them.
  add('tool-numeric-id', { tools: [lookup], replies: [stream(call(0, { id: 7, function: { name: 'lookup', arguments: '{"query":"x"}' } }), done), stream(delta('Done.'), done)], toolReplies: [text('r')] })
  add('tool-numeric-name', { tools: [lookup], replies: [stream(call(0, { id: 'call_1', function: { name: 5, arguments: '{}' } }), done), stream(delta('Done.'), done)] })
  add('tool-boolean-name', { tools: [lookup], replies: [stream(call(0, { id: 'call_1', function: { name: true, arguments: '{}' } }), done), stream(delta('Done.'), done)] })
  add('tool-object-id', { tools: [lookup], replies: [stream(call(0, { id: { n: 1 }, function: { name: 'lookup', arguments: '{"query":"x"}' } }), done), stream(delta('Done.'), done)], toolReplies: [text('r')] })
  add('tool-infinite-id-becomes-null', {
      tools: [lookup],
      replies: [stream('data: {"choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"id":1e999,"function":{"name":"lookup","arguments":"{\\"query\\":\\"x\\"}"}}]}}]}\n\n', done), stream(delta('Done.'), done)],
      toolReplies: [text('r')],
  })
  add('tool-infinite-name-becomes-null', {
    tools: [lookup],
    replies: [stream('data: {"choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"id":"call_1","function":{"name":1e999,"arguments":"{}"}}]}}]}\n\n', done), stream(delta('Done.'), done)],
  })
  add('tool-arguments-invalid-json', { tools: [lookup], replies: [stream(call(0, { id: 'call_1', function: { name: 'lookup', arguments: 'oops' } }), done), stream(delta('Done.'), done)] })
  add('tool-arguments-invalid-json-without-tools', { replies: [stream(call(0, { id: 'call_1', function: { name: 'lookup', arguments: 'oops' } }), done), stream(delta('Done.'), done)] })
  add('tool-calls-string', { tools: [lookup], replies: [stream(delta('Hm. '), ev({ choices: [{ index: 0, delta: { tool_calls: 'abc' } }] }), done), stream(delta('Next.'), done)] })
  add('tool-calls-string-then-real-call', {
      tools: [lookup],
      replies: [stream(ev({ choices: [{ index: 0, delta: { tool_calls: 'abc' } }] }), call(0, { id: 'call_1', function: { name: 'lookup', arguments: '{"query":"x"}' } }), done), stream(delta('Next.'), done)],
      toolReplies: [text('r')],
  })
  add('tool-calls-number-ignored', { tools: [lookup], replies: [stream(delta('A'), ev({ choices: [{ index: 0, delta: { tool_calls: 5 } }] }), delta('B'), done)] })
  add('tool-index-string-key', {
      tools: [lookup],
      replies: [stream(call('b', { id: 'call_1', function: { name: 'lookup', arguments: '{"query":"x"}' } }), done), stream(delta('Done.'), done)], toolReplies: [text('r')],
  })

  // extractJson with tool calls: `__tool_calls` goes through extractJSON, and the result is parsed by wrapToolStream.
  const callEvent = call(0, { id: 'call_1', function: { name: 'lookup', arguments: '{"query":"x"}' } })
  add('extract-json-with-tool-calls-dropped', { extractJson: 'answer', schema, tools: [lookup], replies: [stream(delta('{"answer":"yes"}'), callEvent, done)] })
  add('extract-json-with-tool-calls-key-zero-errors', { extractJson: '0', schema, tools: [lookup], replies: [stream(delta('{"0":"yes"}'), callEvent, done)] })
  add('extract-json-with-tool-calls-nested-errors', { extractJson: '0.id', schema, tools: [lookup], replies: [stream(delta('{"x":1}'), callEvent, done)] })

  // Inputs the decoder does not model decline: before the first snapshot the owner falls back to TS, afterwards the
  // request ends with a terminal failure.
  const proto = ev({ choices: [{ index: '__proto__', delta: { content: 'x' } }] })
  add('multigen-inherited-index-first-chunk', { db: { genTime: 2 }, reply: stream(proto, done), declines: 'stream-inherited-key' })
  add('multigen-inherited-index-after-snapshot', { db: { genTime: 2 }, reply: stream(delta('A'), proto, done), declines: 'stream-inherited-key', terminal: true })
  add('multigen-tool-calls-key-index', { db: { genTime: 2 }, reply: stream(ev({ choices: [{ index: '__tool_calls', delta: { content: 'x' } }] }), done), declines: 'stream-inherited-key' })
  add('tool-index-inherited-name-after-snapshot', {
      tools: [lookup], reply: stream(delta('A'), call('constructor', { id: 'call_1', function: { name: 'lookup', arguments: '{}' } }), done),
      declines: 'stream-inherited-key', terminal: true,
  })
  add('tool-index-inherited-name-in-follow-up', {
      tools: [lookup],
      replies: [stream(callEvent, done), stream(delta('Again. '), call('toString', { id: 'call_2', function: { name: 'lookup', arguments: '{}' } }), done)],
      toolReplies: [text('r')], declines: 'stream-inherited-key', terminal: true,
  })

  // extractThoughts joins reasoning with a newline only when some is already collected.
  add('tool-thoughts-block-empty-then-filled', {
      db: flags('hasFullSystemPrompt', 'deepSeekThinkingInput'), tools: [lookup],
      replies: [stream(delta('<Thoughts>\n</Thoughts>\n<Thoughts>\nabc\n</Thoughts>\nhi'), callEvent, done), stream(delta('Fine.'), done)], toolReplies: [text('r')],
  })
  add('tool-thoughts-two-blocks', {
      db: flags('hasFullSystemPrompt', 'deepSeekThinkingInput'), tools: [lookup],
      replies: [stream(delta('<Thoughts>\nfirst\n</Thoughts>\nmid <Thoughts>\nsecond\n</Thoughts>\nend'), callEvent, done), stream(delta('Fine.'), done)], toolReplies: [text('r')],
  })

  writeFileSync(`${out}/streams.cases.json`, JSON.stringify({ schema: 1, baseDb, cases }, null, 2) + '\n')
}

const baseDb = writeRequests()
writeResponses(baseDb)
writeStreams(baseDb)
