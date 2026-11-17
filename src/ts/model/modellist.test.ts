import { expect, it, vi } from 'vitest'

vi.mock('../storage/database.svelte', () => ({ getDatabase: () => ({}) }))
vi.mock('../globalApi.svelte', () => ({ fetchNative: vi.fn() }))
vi.mock('../stores.svelte', () => ({ DBState: { db: {} } }))
vi.mock('../plugins/plugins.svelte', async () => {
    const { writable } = await import('svelte/store')
    return { customProviderStore: writable([]), pluginV2: { providerOptions: new Map() } }
})
vi.mock('../plugins/apiV3/v3.svelte', () => ({ customV3ProviderMetaStore: [] }))

import { LLMModels, LLMProvider } from './modellist'

it('lists each Gemini model and its generated Vertex entry under one ID', () => {
    const ids = LLMModels
        .filter((model) => model.provider === LLMProvider.GoogleCloud || model.provider === LLMProvider.VertexAI)
        .map((model) => model.id)
    const duplicates = ids.filter((id, index) => ids.indexOf(id) !== index)

    expect(duplicates).toEqual([])
    expect(ids).toContain('gemini-3.8-flash')
    expect(ids).toContain('gemini-3.8-flash-vertex')
})
