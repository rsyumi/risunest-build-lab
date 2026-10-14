import { globalFetch } from "src/ts/globalApi.svelte";
import { getDatabase } from "src/ts/storage/database.svelte";
import { contextHash, type VectorArray } from "./hypamemory";

export interface ContextualEmbeddingProvider {
  readonly modelId: string;
  embedDocumentGroups(groups: string[][], signal?: AbortSignal): Promise<VectorArray[][]>;
  embedQueries(queries: string[], signal?: AbortSignal): Promise<VectorArray[]>;
  getCacheKeySuffix(contextTexts?: string[]): string;
}

export function isContextModel(model: string): boolean {
  return model === 'voyageContext3' || model === 'voyageContext4';
}

export function getContextProvider(model: string): ContextualEmbeddingProvider | null {
  switch (model) {
    case 'voyageContext3':
      return new VoyageContextProvider('voyage-context-3', 'voyageContext3', 120000);
    case 'voyageContext4':
      return new VoyageContextProvider('voyage-context-4', 'voyageContext4', 32000);
    default:
      return null;
  }
}

const VOYAGE_API_URL = "https://api.voyageai.com/v1/contextualizedembeddings";
const MAX_CHUNKS_PER_REQUEST = 16000;
const MAX_INPUTS_PER_REQUEST = 1000;
const TOKEN_OVERHEAD_PER_CHUNK = 64;
const textEncoder = new TextEncoder();

class VoyageContextProvider implements ContextualEmbeddingProvider {
  constructor(
    readonly modelId: string,
    private readonly cacheKey: string,
    private readonly maxTokensPerRequest: number
  ) {}

  private getApiKey(): string {
    const db = getDatabase();
    const apiKey = db.voyageApiKey?.trim();
    if (!apiKey) {
      throw new Error(`${this.modelId} requires a Voyage API Key`);
    }
    return apiKey;
  }

  async embedDocumentGroups(groups: string[][], signal?: AbortSignal): Promise<VectorArray[][]> {
    return this.embedGroups(groups, 'document', signal);
  }

  async embedQueries(queries: string[], signal?: AbortSignal): Promise<VectorArray[]> {
    const results = await this.embedGroups(queries.map(text => [text]), 'query', signal);
    return results.map(group => group[0]);
  }

  private async embedGroups(groups: string[][], inputType: 'document' | 'query', signal?: AbortSignal): Promise<VectorArray[][]> {
    signal?.throwIfAborted();
    const apiKey = this.getApiKey();
    const batches = this.batchGroups(groups);
    const allResults: VectorArray[][] = new Array(groups.length);

    let groupOffset = 0;
    for (const batch of batches) {
      signal?.throwIfAborted();
      const response = await globalFetch(VOYAGE_API_URL, {
        abortSignal: signal,
        headers: {
          "Authorization": "Bearer " + apiKey,
          "Content-Type": "application/json"
        },
        body: {
          "model": this.modelId,
          "inputs": batch,
          "input_type": inputType
        }
      });

      signal?.throwIfAborted();
      if (!response.ok || !response.data.data) {
        throw new Error(JSON.stringify(response.data));
      }

      for (let i = 0; i < batch.length; i++) {
        const groupEmbeddings: VectorArray[] = response.data.data[i].data.map(
          (item: { embedding: VectorArray }) => item.embedding
        );
        allResults[groupOffset + i] = groupEmbeddings;
      }

      groupOffset += batch.length;
    }

    return allResults;
  }

  getCacheKeySuffix(contextTexts?: string[]): string {
    const ctxPart = contextTexts && contextTexts.length > 1
      ? `|ctx:${contextHash(contextTexts)}`
      : '';
    return `|${this.cacheKey}${ctxPart}`;
  }

  private batchGroups(groups: string[][]): string[][][] {
    const batches: string[][][] = [];
    let currentBatch: string[][] = [];
    let currentChunkCount = 0;
    let currentTokenBound = 0;

    for (const group of groups) {
      // Voyage uses NFC-normalized byte-level BPE. Bytes bound text tokens;
      // leave room for the input-type prompt and chunk framing as well.
      const groupTokenBound = group.reduce((total, text) =>
        total + textEncoder.encode(text.normalize('NFC')).length + TOKEN_OVERHEAD_PER_CHUNK, 0);
      if (
        currentBatch.length > 0 &&
        (currentBatch.length + 1 > MAX_INPUTS_PER_REQUEST ||
         currentChunkCount + group.length > MAX_CHUNKS_PER_REQUEST ||
         currentTokenBound + groupTokenBound > this.maxTokensPerRequest)
      ) {
        batches.push(currentBatch);
        currentBatch = [];
        currentChunkCount = 0;
        currentTokenBound = 0;
      }
      // Keep contextual groups intact. An over-budget group travels alone so
      // the API can apply its exact token count instead of rejecting an estimate.
      currentBatch.push(group);
      currentChunkCount += group.length;
      currentTokenBound += groupTokenBound;
    }

    if (currentBatch.length > 0) {
      batches.push(currentBatch);
    }

    return batches;
  }
}
