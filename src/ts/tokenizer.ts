import type { Tiktoken } from "@dqbd/tiktoken";
import type { Tokenizer } from "@mlc-ai/web-tokenizers";
import {  type character, type Chat, type Database, getCurrentCharacter, getDatabase } from "./storage/database.svelte";
import type { MultiModal, OpenAIChat } from "./process/index.svelte";
import { supportsInlayImage } from "./process/files/inlays";
import { risuChatParser } from "./parser/parser.svelte";
import { tokenizeTransformers } from "./process/transformers";
import { globalFetch } from "./globalApi.svelte";
import { getModelInfo, LLMTokenizer, type LLMModel } from "./model/modellist";
import { pluginV2 } from "./plugins/plugins.svelte";
import { LRUMap } from 'mnemonist';
import { isTauri } from './platform';
import { encodeGemma as gemmaTokenize } from './tokenizer/gemmaTokenizer';
import { isNativeModelTokenizerId } from './tokenizer/nativeTokenizer';
import {
    isNativeTokenizerUnavailable,
    resolveProductionNativeTokenizerId,
    tryNativeTokenizerIdsBatch,
    type ProductionNativeTokenizerContext,
    type ProductionNativeTokenizerId,
} from './tokenizer/nativeTokenizerProduction';
import { nativeTokenizerCounts, nativeTokenizerCountsAhead, nativeTokenizerIds } from './tokenizer/nativeTokenizerCoalescer';

const MAX_CACHE_SIZE = 1500;

// IDs, or a count the native tokenizer returned without them.
const encodeCache = new LRUMap<string, number[] | Uint32Array | Int32Array | number>(MAX_CACHE_SIZE);

function getHash(
    data: string,
    aiModel: string,
    customTokenizer: string,
    currentPluginProvider: string,
    googleClaudeTokenizing: boolean,
    modelInfo: LLMModel,
    pluginTokenizer: string
): string {
    const combined = `${data}::${aiModel}::${customTokenizer}::${currentPluginProvider}::${googleClaudeTokenizing ? '1' : '0'}::${modelInfo.tokenizer}::${pluginTokenizer}`;
    return combined;
}


export const tokenizerList = [
    ['tik', 'Tiktoken (OpenAI)'],
    ['mistral', 'Mistral'],
    ['novelai', 'NovelAI'],
    ['claude', 'Claude'],
    ['llama', 'Llama'],
    ['llama3', 'Llama3'],
    ['novellist', 'Novellist'],
    ['gemma', 'Gemma'],
    ['cohere', 'Cohere'],
    ['deepseek', 'DeepSeek'],
    ['deepseek-v4', 'DeepSeek V4'],
    ['glm4', 'GLM4'],
    ['glm5', 'GLM5'],
] as const

export async function encodeWithTokenizer(data: string, tokenizerType: string): Promise<(number[] | Uint32Array | Int32Array)> {
    const nativeTokenizerId = encodeWithTokenizerNativeId(tokenizerType)
    if (nativeTokenizerId) {
        return (await nativeTokenizerIds(nativeTokenizerId, [data], (text) => encodeWithTokenizerInJavaScript(text, tokenizerType)))[0]
    }
    return await encodeWithTokenizerInJavaScript(data, tokenizerType)
}

// The native tokenizer encodeWithTokenizer uses for `tokenizerType`, or null where it runs in JavaScript.
function encodeWithTokenizerNativeId(tokenizerType: string): ProductionNativeTokenizerId | null {
    return routableNativeTokenizerId({
        isTauri,
        aiModel: '',
        customTokenizer: '',
        // encodeWithTokenizerInJavaScript reads any other name as cl100k_base.
        modelTokenizerId: isNativeModelTokenizerId(tokenizerType) ? tokenizerType
            : tokenizerType === 'o200k_base' ? 'o200k_base' : 'cl100k_base',
    })
}

// Each text's `encodeWithTokenizer(text, tokenizerType).length`. A native tokenizer counts the texts in one request; in
// JavaScript they are encoded in turn.
async function encodeWithTokenizerCounts(texts: string[], tokenizerType: string): Promise<number[]> {
    const nativeTokenizerId = encodeWithTokenizerNativeId(tokenizerType)
    if (nativeTokenizerId) {
        return await nativeTokenizerCounts(nativeTokenizerId, texts, (text) => encodeWithTokenizerInJavaScript(text, tokenizerType))
    }
    const counts: number[] = []
    for (const text of texts) {
        counts.push((await encodeWithTokenizerInJavaScript(text, tokenizerType)).length)
    }
    return counts
}

// encodeWithTokenizerCounts for the texts a native tokenizer answers, and undefined for the rest.
async function encodeWithTokenizerCountsAhead(texts: string[], tokenizerType: string): Promise<(number | undefined)[]> {
    const nativeTokenizerId = encodeWithTokenizerNativeId(tokenizerType)
    return nativeTokenizerId ? await nativeTokenizerCountsAhead(nativeTokenizerId, texts) : texts.map(() => undefined)
}

async function encodeWithTokenizerInJavaScript(data: string, tokenizerType: string): Promise<(number[] | Uint32Array | Int32Array)> {
    switch (tokenizerType) {
        case 'tik':
            return await tikJS(data, 'cl100k_base');
        case 'o200k_base':
            return await tikJS(data, 'o200k_base');
        case 'mistral':
            return await tokenizeWebTokenizers(data, 'mistral');
        case 'novelai':
            return await tokenizeWebTokenizers(data, 'novelai');
        case 'claude':
            return await tokenizeWebTokenizers(data, 'claude');
        case 'llama':
            return await tokenizeWebTokenizers(data, 'llama');
        case 'llama3':
            return await tokenizeWebTokenizers(data, 'llama3');
        case 'novellist':
            return await tokenizeWebTokenizers(data, 'novellist');
        case 'gemma':
            return await gemmaTokenize(data);
        case 'cohere':
            return await tokenizeWebTokenizers(data, 'cohere');
        case 'deepseek':
            return await tokenizeWebTokenizers(data, 'DeepSeek');
        case 'deepseek-v4':
            return await tokenizeWebTokenizers(data, 'DeepSeekV4');
        case 'glm4':
            return await tokenizeWebTokenizers(data, 'GLM4');
        case 'glm5':
            return await tokenizeWebTokenizers(data, 'GLM5');
        default:
            return await tikJS(data, 'cl100k_base');
    }
}

export async function encode(data:string):Promise<(number[]|Uint32Array|Int32Array)>{
    const db = getDatabase();
    const modelInfo = getModelInfo(db.aiModel);
    const pluginTokenizer = pluginV2.providerOptions.get(db.currentPluginProvider)?.tokenizer ?? "none";

    let cacheKey = ''
    if(db.useTokenizerCaching){
        cacheKey = getHash(
            data,
            db.aiModel,
            db.customTokenizer,
            db.currentPluginProvider,
            db.googleClaudeTokenizing,
            modelInfo,
            pluginTokenizer
        );
        const cachedResult = encodeCache.get(cacheKey);
        if (cachedResult !== undefined && typeof cachedResult !== 'number') {
            return cachedResult;
        }
    }

    const nativeTokenizerId = encodeNativeTokenizerId(db, modelInfo, pluginTokenizer)
    const result = nativeTokenizerId
        ? (await nativeTokenizerIds(nativeTokenizerId, [data], (text) => encodeInJavaScript(text, db, modelInfo, pluginTokenizer)))[0]
        : await encodeInJavaScript(data, db, modelInfo, pluginTokenizer)
    if(db.useTokenizerCaching){
        encodeCache.set(cacheKey, result);
    }

    return result;
}

// The token count of each text, as `encode(text).length` gives it. A native tokenizer returns the counts without the
// IDs, and the texts requested in the same turn go out together; in JavaScript the texts are encoded in turn.
export async function encodeCounts(texts:string[]):Promise<number[]>{
    return await countEncodings(texts, true) as number[]
}

// encodeCounts for the texts a native tokenizer answers, and undefined for the rest. A text it does not answer, and every
// text where counts run in JavaScript, stays uncounted, so counting ahead of a walk does no JavaScript work the walk would
// not do.
async function encodeCountsAhead(texts:string[]):Promise<(number | undefined)[]>{
    return await countEncodings(texts, false)
}

async function countEncodings(texts:string[], countRestInJavaScript:boolean):Promise<(number | undefined)[]>{
    const db = getDatabase();
    const modelInfo = getModelInfo(db.aiModel);
    const pluginTokenizer = pluginV2.providerOptions.get(db.currentPluginProvider)?.tokenizer ?? "none";
    const nativeTokenizerId = encodeNativeTokenizerId(db, modelInfo, pluginTokenizer)
    if(!nativeTokenizerId){
        if(!countRestInJavaScript){
            return texts.map(() => undefined)
        }
        const counts:number[] = []
        for(const text of texts){
            counts.push((await encode(text)).length)
        }
        return counts
    }

    const counts = new Array<number | undefined>(texts.length)
    const cacheKeys:string[] = []
    const missing:number[] = []
    for(const [index, text] of texts.entries()){
        if(db.useTokenizerCaching){
            cacheKeys[index] = getHash(
                text,
                db.aiModel,
                db.customTokenizer,
                db.currentPluginProvider,
                db.googleClaudeTokenizing,
                modelInfo,
                pluginTokenizer
            );
            const cachedResult = encodeCache.get(cacheKeys[index]);
            if(cachedResult !== undefined){
                counts[index] = typeof cachedResult === 'number' ? cachedResult : cachedResult.length
                continue
            }
        }
        missing.push(index)
    }
    if(missing.length > 0){
        const missingTexts = missing.map((index) => texts[index])
        const missingCounts = countRestInJavaScript
            ? await nativeTokenizerCounts(
                nativeTokenizerId,
                missingTexts,
                (text) => encodeInJavaScript(text, db, modelInfo, pluginTokenizer),
            )
            : await nativeTokenizerCountsAhead(nativeTokenizerId, missingTexts)
        missing.forEach((index, position) => {
            const count = missingCounts[position]
            counts[index] = count
            // IDs cached for the text stay, since they answer a count too.
            if(count !== undefined && db.useTokenizerCaching && typeof (encodeCache.peek(cacheKeys[index]) ?? 0) === 'number'){
                encodeCache.set(cacheKeys[index], count)
            }
        })
    }
    return counts
}

// The token IDs of texts a loop then encodes one at a time, as `encode` gives them. Where the native tokenizer runs
// they come from one request, keyed by text; in JavaScript the map is empty and the loop encodes each text as it goes.
export async function prefetchTokenIds(texts:string[]):Promise<Map<string, number[] | Uint32Array | Int32Array>>{
    const ids = new Map<string, number[] | Uint32Array | Int32Array>()
    const db = getDatabase();
    const modelInfo = getModelInfo(db.aiModel);
    const pluginTokenizer = pluginV2.providerOptions.get(db.currentPluginProvider)?.tokenizer ?? "none";
    const nativeTokenizerId = encodeNativeTokenizerId(db, modelInfo, pluginTokenizer)
    if(!nativeTokenizerId){
        return ids
    }
    const cacheKey = (text:string) => getHash(
        text,
        db.aiModel,
        db.customTokenizer,
        db.currentPluginProvider,
        db.googleClaudeTokenizing,
        modelInfo,
        pluginTokenizer
    )
    const missing:string[] = []
    for(const text of new Set(texts)){
        const cachedResult = db.useTokenizerCaching ? encodeCache.get(cacheKey(text)) : undefined
        if(cachedResult !== undefined && typeof cachedResult !== 'number'){
            ids.set(text, cachedResult)
        }
        else{
            missing.push(text)
        }
    }
    const encoded = await nativeTokenizerIds(
        nativeTokenizerId,
        missing,
        (text) => encodeInJavaScript(text, db, modelInfo, pluginTokenizer),
    )
    missing.forEach((text, index) => {
        ids.set(text, encoded[index])
        if(db.useTokenizerCaching){
            encodeCache.set(cacheKey(text), encoded[index])
        }
    })
    return ids
}

// Counts what a loop adds, in order. Where counts run in JavaScript, `add` counts the item at once and returns its
// count, so tokenizer calls keep their place among the loop's other work. For a native tokenizer, `add` returns 0 and
// `settle` counts every deferred item as one batch, unless the batch is `inline`. An item's count is what `add`
// returned plus its entry in `settle`.
export class TokenCountBatch<T> {
    private readonly deferred:{ index:number, item:T }[] = []
    private readonly counted:number[] = []
    private added = 0

    constructor(
        private readonly countOne:(item:T) => Promise<number>,
        private readonly countAll:(items:T[]) => Promise<number[]>,
        private readonly inline = false,
    ){}

    async add(item:T):Promise<number>{
        const index = this.added++
        if(!this.inline && currentNativeTokenizerId() !== null){
            this.deferred.push({ index, item })
            return 0
        }
        return await this.countOne(item)
    }

    // Counts the items deferred so far, so a tokenizer error surfaces before the loop does something that outlives it.
    async flush():Promise<void>{
        const deferred = this.deferred.splice(0)
        if(deferred.length === 0){
            return
        }
        const counts = await this.countAll(deferred.map((entry) => entry.item))
        deferred.forEach((entry, position) => { this.counted[entry.index] = counts[position] })
    }

    async settle():Promise<number[]>{
        await this.flush()
        const rest = Array.from({ length: this.added }, (_, index) => this.counted[index] ?? 0)
        this.counted.length = 0
        return rest
    }
}

// Texts a loop adds one at a time, counted as `tokenize` counts them.
export function tokenCountBatch():TokenCountBatch<string>{
    return new TokenCountBatch(tokenize, encodeCounts)
}

// Counts for a walk that stops at a budget. Where counts run natively, `prefetch` counts the texts the walk can reach
// next in one request and `count` answers from them; in JavaScript `prefetch` does nothing, and neither does it for a
// text the native tokenizer does not answer, so the walk counts only the texts it reaches.
export class TokenCounter {
    private counts = new Map<string, number>()
    private countsTokenizerId:ProductionNativeTokenizerId | null = null

    constructor(
        private readonly nativeTokenizerId:() => ProductionNativeTokenizerId | null,
        private readonly countOne:(text:string) => Promise<number>,
        private readonly countMany:(texts:string[]) => Promise<number[]>,
        private readonly countAhead:(texts:string[]) => Promise<(number | undefined)[]>,
    ){}

    async prefetch(texts:string[]):Promise<void>{
        const tokenizerId = this.nativeTokenizerId()
        if(tokenizerId === null){
            return
        }
        const counts = this.countsFor(tokenizerId)
        const missing = [...new Set(texts.filter((text) => !counts.has(text)))]
        if(missing.length === 0){
            return
        }
        const missingCounts = await this.countAhead(missing)
        missing.forEach((text, index) => {
            const count = missingCounts[index]
            if(count !== undefined){
                counts.set(text, count)
            }
        })
    }

    async count(text:string):Promise<number>{
        const counted = this.countsTokenizerId !== null && this.countsTokenizerId === this.nativeTokenizerId()
            ? this.counts.get(text)
            : undefined
        return counted ?? await this.countOne(text)
    }

    // Each text's count as `count` gives it. Natively, the texts without a count go out in one request and are kept.
    async countAll(texts:string[]):Promise<number[]>{
        const tokenizerId = this.nativeTokenizerId()
        if(tokenizerId === null){
            return await this.countMany(texts)
        }
        return await this.countNatively(tokenizerId, texts)
    }

    // The counts kept for `tokenizerId`, emptied when the tokenizer changes.
    private countsFor(tokenizerId:ProductionNativeTokenizerId):Map<string, number>{
        if(tokenizerId !== this.countsTokenizerId){
            this.counts = new Map()
            this.countsTokenizerId = tokenizerId
        }
        return this.counts
    }

    private async countNatively(tokenizerId:ProductionNativeTokenizerId, texts:string[]):Promise<number[]>{
        const counts = this.countsFor(tokenizerId)
        const missing = [...new Set(texts.filter((text) => !counts.has(text)))]
        if(missing.length > 0){
            const missingCounts = await this.countMany(missing)
            missing.forEach((text, index) => counts.set(text, missingCounts[index]))
        }
        return texts.map((text) => counts.get(text)!)
    }
}

// A counter for the selected model's tokenizer, as `tokenize` counts, or for `tokenizerType` as encodeWithTokenizer
// reads it.
export function tokenCounter(tokenizerType?:string):TokenCounter{
    if(tokenizerType === undefined){
        return new TokenCounter(currentNativeTokenizerId, tokenize, encodeCounts, encodeCountsAhead)
    }
    return new TokenCounter(
        () => encodeWithTokenizerNativeId(tokenizerType),
        async (text) => (await encodeWithTokenizerCounts([text], tokenizerType))[0],
        (texts) => encodeWithTokenizerCounts(texts, tokenizerType),
        (texts) => encodeWithTokenizerCountsAhead(texts, tokenizerType),
    )
}

// Runs counts that do not depend on each other. Where counts run natively they start together, so their texts go out
// in one request; in JavaScript they run in turn, as one tokenizer model is loaded at a time.
export async function countTogether<T extends unknown[]>(...tasks:{ [K in keyof T]: () => Promise<T[K]> }):Promise<T>{
    const runs = tasks as (() => Promise<unknown>)[]
    if(currentNativeTokenizerId() !== null){
        return await Promise.all(runs.map((task) => task())) as T
    }
    const results:unknown[] = []
    for(const task of runs){
        results.push(await task())
    }
    return results as T
}

// The native tokenizer for a context, or null where it runs in JavaScript, including a family whose native tokenizer
// failed for this session.
function routableNativeTokenizerId(context:ProductionNativeTokenizerContext):ProductionNativeTokenizerId | null {
    const tokenizerId = resolveProductionNativeTokenizerId(context)
    return tokenizerId !== null && !isNativeTokenizerUnavailable(tokenizerId) ? tokenizerId : null
}

function encodeNativeTokenizerId(db:Database, modelInfo:LLMModel, pluginTokenizer:string):ProductionNativeTokenizerId | null {
    return routableNativeTokenizerId({
        isTauri,
        aiModel: db.aiModel,
        customTokenizer: db.customTokenizer,
        modelTokenizerId: nativeModelTokenizerId(modelInfo.tokenizer, db.googleClaudeTokenizing),
        pluginTokenizer,
    })
}

// The native tokenizer counts use for the current settings, or null where they run in JavaScript.
function currentNativeTokenizerId():ProductionNativeTokenizerId | null {
    const db = getDatabase()
    return encodeNativeTokenizerId(
        db,
        getModelInfo(db.aiModel),
        pluginV2.providerOptions.get(db.currentPluginProvider)?.tokenizer ?? "none",
    )
}

async function encodeInJavaScript(data:string, db:Database, modelInfo:LLMModel, pluginTokenizer:string):Promise<(number[]|Uint32Array|Int32Array)>{
    let result: number[] | Uint32Array | Int32Array;

    if(db.aiModel === 'openrouter' || db.aiModel === 'reverse_proxy'){
        switch(db.customTokenizer){
            case 'mistral':
                result = await tokenizeWebTokenizers(data, 'mistral'); break;
            case 'llama':
                result = await tokenizeWebTokenizers(data, 'llama'); break;
            case 'novelai':
                result = await tokenizeWebTokenizers(data, 'novelai'); break;
            case 'claude':
                result = await tokenizeWebTokenizers(data, 'claude'); break;
            case 'novellist':
                result = await tokenizeWebTokenizers(data, 'novellist'); break;
            case 'llama3':
                result = await tokenizeWebTokenizers(data, 'llama3'); break;
            case 'gemma':
                result = await gemmaTokenize(data); break;
            case 'cohere':
                result = await tokenizeWebTokenizers(data, 'cohere'); break;
            case 'deepseek':
                result = await tokenizeWebTokenizers(data, 'DeepSeek'); break;
            case 'deepseek-v4':
                result = await tokenizeWebTokenizers(data, 'DeepSeekV4'); break;
            case 'glm4':
                result = await tokenizeWebTokenizers(data, 'GLM4'); break;
            case 'glm5':
                result = await tokenizeWebTokenizers(data, 'GLM5'); break;
            default:
                result = await tikJS(data, 'o200k_base'); break;
        }
    } else if (db.aiModel === 'custom' && pluginTokenizer) {
        switch(pluginTokenizer){
            case 'mistral':
                result = await tokenizeWebTokenizers(data, 'mistral'); break;
            case 'llama':
                result = await tokenizeWebTokenizers(data, 'llama'); break;
            case 'novelai':
                result = await tokenizeWebTokenizers(data, 'novelai'); break;
            case 'claude':
                result = await tokenizeWebTokenizers(data, 'claude'); break;
            case 'novellist':
                result = await tokenizeWebTokenizers(data, 'novellist'); break;
            case 'llama3':
                result = await tokenizeWebTokenizers(data, 'llama3'); break;
            case 'gemma':
                result = await gemmaTokenize(data); break;
            case 'cohere':
                result = await tokenizeWebTokenizers(data, 'cohere'); break;
            case 'deepseek':
                result = await tokenizeWebTokenizers(data, 'DeepSeek'); break;
            case 'deepseek-v4':
                result = await tokenizeWebTokenizers(data, 'DeepSeekV4'); break;
            case 'glm4':
                result = await tokenizeWebTokenizers(data, 'GLM4'); break;
            case 'glm5':
                result = await tokenizeWebTokenizers(data, 'GLM5'); break;
            case 'o200k_base':
                result = await tikJS(data, 'o200k_base'); break;
            case 'cl100k_base':
                result = await tikJS(data, 'cl100k_base'); break;
            case 'custom':
                result = await pluginV2.providerOptions.get(db.currentPluginProvider)?.tokenizerFunc?.(data) ?? [0]; break;
            default:
                result = await tikJS(data, 'o200k_base'); break; 
        }
    } 
    
    // Fallback
    if (result === undefined) {
        if(modelInfo.tokenizer === LLMTokenizer.NovelList){
            result = await tokenizeWebTokenizers(data, 'novellist');
        } else if(modelInfo.tokenizer === LLMTokenizer.Claude){
            result = await tokenizeWebTokenizers(data, 'claude');
        } else if(modelInfo.tokenizer === LLMTokenizer.NovelAI){
            result = await tokenizeWebTokenizers(data, 'novelai');
        } else if(modelInfo.tokenizer === LLMTokenizer.Mistral){
            result = await tokenizeWebTokenizers(data, 'mistral');
        } else if(modelInfo.tokenizer === LLMTokenizer.Llama){
            result = await tokenizeWebTokenizers(data, 'llama');
        } else if(modelInfo.tokenizer === LLMTokenizer.Local){
            if (!db.aiModel.startsWith("hf:::")) {
                throw new Error(
                    "Local tokenization requires a Hugging Face model (hf:::).",
                );
            }
            result = await tokenizeTransformers(data, db.aiModel.slice(5));
        } else if(modelInfo.tokenizer === LLMTokenizer.tiktokenO200Base){
            result = await tikJS(data, 'o200k_base');
        } else if(modelInfo.tokenizer === LLMTokenizer.GoogleCloud && db.googleClaudeTokenizing){
            result = await tokenizeGoogleCloud(data);
        } else if(modelInfo.tokenizer === LLMTokenizer.Gemma || modelInfo.tokenizer === LLMTokenizer.GoogleCloud){
            result = await gemmaTokenize(data);
        } else if(modelInfo.tokenizer === LLMTokenizer.DeepSeek){
            result = await tokenizeWebTokenizers(data, 'DeepSeek');
        } else if(modelInfo.tokenizer === LLMTokenizer.DeepSeekV4){
            result = await tokenizeWebTokenizers(data, 'DeepSeekV4');
        } else if(modelInfo.tokenizer === LLMTokenizer.GLM4){
            result = await tokenizeWebTokenizers(data, 'GLM4');
        } else if(modelInfo.tokenizer === LLMTokenizer.GLM5){
            result = await tokenizeWebTokenizers(data, 'GLM5');
        } else if(modelInfo.tokenizer === LLMTokenizer.Cohere){
            result = await tokenizeWebTokenizers(data, 'cohere');
        } else {
            result = await tikJS(data);
        }
    }

    return result;
}

type tokenizerType = 'novellist'|'claude'|'novelai'|'llama'|'mistral'|'llama3'|'cohere'|'googleCloud'|'DeepSeek'|'DeepSeekV4'|'GLM4'|'GLM5'

let tikParser:Tiktoken = null
let tokenizersTokenizer:Tokenizer = null
let tokenizersType:tokenizerType = null
let lastTikModel = 'cl100k_base'

let googleCloudTokenizedCache = new Map<string, number>()

async function tokenizeGoogleCloud(text:string) {
    const db = getDatabase()
    const model = getModelInfo(db.aiModel)
    const cacheKey = text + model.internalID

    if(googleCloudTokenizedCache.has(cacheKey)){
        const count = googleCloudTokenizedCache.get(cacheKey) ?? 0
        return new Uint32Array(count)
    }

    const res = await fetch(`https://generativelanguage.googleapis.com/v1beta/models/${model.internalID}:countTokens?key=${db.google?.accessToken}`, {
        method: 'POST',
        headers: {
            "Content-Type": "application/json",
        },
        body: JSON.stringify({
            contents: [{
                parts:[{
                    text: text
                }]
            }]
        }),
    })

    if(res.status !== 200){
        return await gemmaTokenize(text)
    }

    const json = await res.json()
    googleCloudTokenizedCache.set(cacheKey, json.totalTokens as number)
    const count = json.totalTokens as number

    return new Uint32Array(count)
}

async function tikJS(text:string, model='cl100k_base') {
    if(!tikParser || lastTikModel !== model){
        tikParser?.free()
        if(model === 'cl100k_base'){
            const {Tiktoken} = await import('@dqbd/tiktoken')
            const cl100k_base = await import("@dqbd/tiktoken/encoders/cl100k_base.json");
            lastTikModel = model   
        
            tikParser = new Tiktoken(
                cl100k_base.bpe_ranks,
                cl100k_base.special_tokens,
                cl100k_base.pat_str
            );
        }
        if(model === 'o200k_base'){
            const {Tiktoken} = await import('@dqbd/tiktoken')
            const o200k_base = await import("src/etc/o200k_base.json");
            lastTikModel = model
            tikParser = new Tiktoken(
                o200k_base.bpe_ranks,
                o200k_base.special_tokens,
                o200k_base.pat_str
            );
        }
    }
    return tikParser.encode(text)
}

async function geminiTokenizer(text:string) {
    const db = getDatabase()
    const fetchResult = await globalFetch(`https://generativelanguage.googleapis.com/v1beta/${db.aiModel}:countTextTokens`, {
        "headers": {
            "content-type": "application/json",
            "authorization": `Bearer ${db.google.accessToken}`
        },
        "body": JSON.stringify({
            "prompt":{
                text: text
            }
        }),
        "method": "POST"
    })

    if(!fetchResult.ok){
        //fallback to tiktoken
        return await tikJS(text)
    }

    const result = fetchResult.data

    return result.tokenCount ?? 0
}

async function tokenizeWebTokenizers(text:string, type:tokenizerType) {
    if(type !== tokenizersType || !tokenizersTokenizer){
        const webTokenizer = await import('@mlc-ai/web-tokenizers')
        let candidate:Tokenizer = null
        switch(type){
            case "novellist":
                candidate = await webTokenizer.Tokenizer.fromSentencePiece(
                    await (await fetch("/token/trin/spiece.model")
                ).arrayBuffer())
                break
            case "claude":
                candidate = await webTokenizer.Tokenizer.fromJSON(
                    await (await fetch("/token/claude/claude.json")
                ).arrayBuffer())
                break
            case 'llama3':
                candidate = await webTokenizer.Tokenizer.fromJSON(
                    await (await fetch("/token/llama/llama3.json")
                ).arrayBuffer())
                break
            case 'cohere':
                candidate = await webTokenizer.Tokenizer.fromJSON(
                    await (await fetch("/token/cohere/tokenizer.json")
                ).arrayBuffer())
                break
            case 'novelai':
                candidate = await webTokenizer.Tokenizer.fromSentencePiece(
                    await (await fetch("/token/nai/nerdstash_v2.model")
                ).arrayBuffer())
                
                break
            case 'llama':
                candidate = await webTokenizer.Tokenizer.fromSentencePiece(
                    await (await fetch("/token/llama/llama.model")
                ).arrayBuffer())
                break
            case 'mistral':
                candidate = await webTokenizer.Tokenizer.fromSentencePiece(
                    await (await fetch("/token/mistral/tokenizer.model")
                ).arrayBuffer())
                break
            case 'DeepSeek':
                candidate = await webTokenizer.Tokenizer.fromJSON(
                    await (await fetch("/token/deepseek/tokenizer.json")
                ).arrayBuffer())
                break
            case 'DeepSeekV4':
                candidate = await webTokenizer.Tokenizer.fromJSON(
                    await (await fetch("/token/deepseek/v4/tokenizer.json")
                ).arrayBuffer())
                break
            case 'GLM4':
                candidate = await webTokenizer.Tokenizer.fromJSON(
                    await (await fetch("/token/glm4/tokenizer.json")
                ).arrayBuffer())
                break
            case 'GLM5':
                candidate = await webTokenizer.Tokenizer.fromJSON(
                    await (await fetch("/token/glm5/tokenizer.json")
                ).arrayBuffer())
                break

        }
        const replaced = tokenizersTokenizer
        tokenizersTokenizer = candidate
        tokenizersType = type
        replaced?.dispose()
    }
    return (tokenizersTokenizer.encode(text))
}

export async function tokenizerChar(char:character) {
    return (await encodeCounts([char.name + '\n' + char.firstMessage + '\n' + char.desc]))[0]
}

export async function tokenize(data:string) {
    return (await encodeCounts([data]))[0]
}

export async function tokenizeAccurate(data:string, consistantChar?:boolean) {
    return (await tokenizeAccurateList([data], consistantChar))[0]
}

// tokenizeAccurate for each text, as one batch where counts run natively.
export async function tokenizeAccurateList(texts:string[], consistantChar?:boolean):Promise<number[]> {
    return await encodeCounts(texts.map((data) => risuChatParser(data.replace('{{slot}}',''), {
        tokenizeAccurate: true,
        consistantChar: consistantChar,
    })))
}


// The native tokenizer for the one `encode` falls back to for the selected model, or null to keep JavaScript: the Google
// count API, a local Hugging Face model, and a value outside LLMTokenizer.
function nativeModelTokenizerId(tokenizer: LLMTokenizer, googleClaudeTokenizing: boolean): ProductionNativeTokenizerId | null {
    switch (tokenizer) {
        case LLMTokenizer.Unknown:
        case LLMTokenizer.tiktokenCl100kBase:
        case LLMTokenizer.Llama3: return 'cl100k_base'
        case LLMTokenizer.tiktokenO200Base: return 'o200k_base'
        case LLMTokenizer.NovelList: return 'novellist'
        case LLMTokenizer.Claude: return 'claude'
        case LLMTokenizer.NovelAI: return 'novelai'
        case LLMTokenizer.Mistral: return 'mistral'
        case LLMTokenizer.Llama: return 'llama'
        case LLMTokenizer.GoogleCloud: return googleClaudeTokenizing ? null : 'gemma'
        case LLMTokenizer.Gemma: return 'gemma'
        case LLMTokenizer.DeepSeek: return 'deepseek'
        case LLMTokenizer.DeepSeekV4: return 'deepseek-v4'
        case LLMTokenizer.GLM4: return 'glm4'
        case LLMTokenizer.GLM5: return 'glm5'
        case LLMTokenizer.Cohere: return 'cohere'
        default: return null
    }
}

export class ChatTokenizer {

    private chatAdditionalTokens:number
    private useName:'name'|'noName'
    private readonly chatCounts = new WeakMap<OpenAIChat, {
        content: string, name: string | undefined, identity: string, count: number,
    }>()
    // Text counts a native tokenizer gave, so a copy of a counted chat, a chat with multimodals, or a text `prefetch`
    // counted needs no request.
    private readonly textCounts = tokenCounter()

    constructor(chatAdditionalTokens:number, useName:'name'|'noName', private reuseChatCounts = false){
        this.chatAdditionalTokens = chatAdditionalTokens
        this.useName = useName
    }
    async tokenizeChat(data:OpenAIChat, args:{
        countThoughts?:boolean,
    } = {}) {
        return (await this.countChats([data], args))[0]
    }
    // Each chat's count as tokenizeChat gives it. A native tokenizer counts the texts of every chat as one batch; in
    // JavaScript the chats are counted one after another.
    async tokenizeChatList(chats:OpenAIChat[], args:{
        countThoughts?:boolean,
    } = {}):Promise<number[]> {
        if(currentNativeTokenizerId() !== null){
            return await this.countChats(chats, args)
        }
        const counts:number[] = []
        for(const chat of chats){
            counts.push((await this.countChats([chat], args))[0])
        }
        return counts
    }
    // Chats a loop adds one at a time, counted as tokenizeChat counts them. An `inline` batch counts each chat as it is
    // added on every route.
    countBatch(options:{ inline?:boolean } = {}):TokenCountBatch<OpenAIChat> {
        return new TokenCountBatch(
            (chat) => this.tokenizeChat(chat),
            (chats) => this.tokenizeChatList(chats),
            options.inline,
        )
    }
    // Counts in one request the texts of chats a loop then counts one at a time, where counts run natively and are
    // reused, so those chats count without a request. In JavaScript it does nothing.
    async prefetch(texts:string[]):Promise<void> {
        if(this.reuseChatCounts){
            await this.textCounts.prefetch(texts)
        }
    }
    // Counts the texts of `chats` in one request. chatCounts answers the chats it holds, and takes the new counts.
    private async countChats(chats:OpenAIChat[], args:{
        countThoughts?:boolean,
    }):Promise<number[]> {
        const counts = new Array<number>(chats.length)
        const pending:{
            index:number, data:OpenAIChat, identity:string | null, content:string, name:string | undefined,
            named:boolean, thoughts:number, start:number,
        }[] = []
        const texts:string[] = []
        for(const [index, data] of chats.entries()){
            const identity = this.reuseChatCounts && !data.multimodals?.length && !args.countThoughts
                ? this.chatCountIdentity()
                : null
            const previous = identity === null ? undefined : this.chatCounts.get(data)
            if (previous?.identity === identity && previous.content === data.content && previous.name === data.name) {
                counts[index] = previous.count
                continue
            }
            const start = texts.length
            texts.push(data.content)
            const named = !!data.name && this.useName === 'name'
            if(named){
                texts.push(data.name)
            }
            const thoughts = data.thoughts && data.thoughts.length > 0 && args.countThoughts ? data.thoughts : []
            texts.push(...thoughts)
            pending.push({
                index, data, identity, content: data.content, name: data.name, named, thoughts: thoughts.length, start,
            })
        }
        const textCounts = this.reuseChatCounts ? await this.textCounts.countAll(texts) : await encodeCounts(texts)
        for(const { index, data, identity, content, name, named, thoughts, start } of pending){
            let position = start
            let encoded = textCounts[position++] + this.chatAdditionalTokens
            if(named){
                encoded += textCounts[position++] + 1
            }
            if(data.multimodals && data.multimodals.length > 0){
                for(const multimodal of data.multimodals){
                    encoded += this.tokenizeMultiModal(multimodal)
                }
            }
            for(let thought = 0; thought < thoughts; thought++){
                encoded += textCounts[position++] + 1
            }
            if (identity !== null && identity === this.chatCountIdentity()
                && content === data.content && name === data.name && !data.multimodals?.length) {
                this.chatCounts.set(data, { content, name, identity, count: encoded })
            }
            counts[index] = encoded
        }
        return counts
    }
    private chatCountIdentity(): string | null {
        const db = getDatabase()
        const model = getModelInfo(db.aiModel)
        const pluginTokenizer = pluginV2.providerOptions.get(db.currentPluginProvider)?.tokenizer ?? 'none'
        if ((db.aiModel === 'custom' && pluginTokenizer === 'custom')
            || (model.tokenizer === LLMTokenizer.GoogleCloud && db.googleClaudeTokenizing)) return null
        return JSON.stringify([
            db.aiModel, db.customTokenizer, db.currentPluginProvider,
            db.googleClaudeTokenizing, model.tokenizer, pluginTokenizer,
        ])
    }
    async tokenizeChats(data:OpenAIChat[]){
        let encoded = 0
        for(const count of await this.tokenizeChatList(data)){
            encoded += count
        }
        return encoded
    }

    tokenizeMultiModal(data:MultiModal){
        const db = getDatabase()
        if(!supportsInlayImage()){
            return this.chatAdditionalTokens
        }
        if(db.gptVisionQuality === 'low'){
            return 87
        }

        let encoded = this.chatAdditionalTokens
        let height = data.height ?? 0
        let width = data.width ?? 0

        if(height === width){
            if(height > 768){
                height = 768
                width = 768
            }
        }
        else if(height > width){
            if(width > 768){
                width = 768
                height = height * (768 / width)
            }
        }
        else{
            if(height > 768){
                height = 768
                width = width * (768 / height)
            }
        }

        const chunkSize = Math.ceil(width / 512) * Math.ceil(height / 512)
        encoded += chunkSize * 2
        encoded += 85

        return encoded
    }
    
}

export async function tokenizeNum(data:string) {
    const encoded = await encode(data)
    return encoded
}

export async function strongBan(data:string, bias:{[key:number]:number}) {

    if(localStorage.getItem('strongBan_' + data)){
        return JSON.parse(localStorage.getItem('strongBan_' + data))
    }
    let charAlt = [
        data,
        data.trim(),
        data.toLocaleUpperCase(),
        data.toLocaleLowerCase(),
        data[0].toLocaleUpperCase() + data.slice(1),
        data[0].toLocaleLowerCase() + data.slice(1),
    ]

    let banChars = " !\"#$%&'()*+,-./:;<=>?@[\\]^_`{|}~“”‘’«»「」…–―※"

    const runJavaScript = async () => {
        let unbanChars:number[] = []
        for(const char of banChars){
            unbanChars.push((await tokenizeNum(char))[0])
        }
        for(const char of banChars){
            const encoded = await tokenizeNum(char)
            if(encoded.length > 0 && !unbanChars.includes(encoded[0])){
                bias[encoded[0]] = -100
            }
            for(const alt of charAlt){
                const encoded = await tokenizeNum(alt + char)
                if(encoded.length > 0 && !unbanChars.includes(encoded[0])){
                    bias[encoded[0]] = -100
                }
                const encoded2 = await tokenizeNum(char + alt)
                if(encoded2.length > 0 && !unbanChars.includes(encoded2[0])){
                    bias[encoded2[0]] = -100
                }
            }
        }
    }

    let banCharacterCount = 0
    for(const _char of banChars){
        banCharacterCount++
    }
    const nativeBatchCandidate = {
        itemCount: banCharacterCount * (2 + charAlt.length * 2),
        aggregateInputBytes: () => {
            const encoder = new TextEncoder()
            let banCharacterBytes = 0
            for(const char of banChars){
                banCharacterBytes += encoder.encode(char).byteLength
            }
            const alternateBytes = charAlt.reduce(
                (total, alternate) => total + encoder.encode(alternate).byteLength,
                0,
            )
            return (
                2 * (charAlt.length + 1) * banCharacterBytes +
                2 * banCharacterCount * alternateBytes
            )
        },
        buildTexts: () => {
            const texts:string[] = []
            for(const char of banChars){
                texts.push(char)
            }
            for(const char of banChars){
                texts.push(char)
                for(const alt of charAlt){
                    texts.push(alt + char, char + alt)
                }
            }
            return texts
        },
    }

    const db = getDatabase()
    const modelInfo = getModelInfo(db.aiModel)
    const modelTokenizerId = nativeModelTokenizerId(modelInfo.tokenizer, db.googleClaudeTokenizing)
    let nativeIds:number[][]|null = null
    try {
        nativeIds = await tryNativeTokenizerIdsBatch(nativeBatchCandidate, {
            isTauri,
            aiModel: db.aiModel,
            customTokenizer: db.customTokenizer,
            modelTokenizerId,
            pluginTokenizer: pluginV2.providerOptions.get(db.currentPluginProvider)?.tokenizer,
        })
    } catch {
        await runJavaScript()
        localStorage.setItem('strongBan_' + data, JSON.stringify(bias))
        return bias
    }

    if(nativeIds){
        let index = 0
        const unbanChars:number[] = []
        for(const _char of banChars){
            unbanChars.push(nativeIds[index++][0])
        }
        for(const _char of banChars){
            const encoded = nativeIds[index++]
            if(encoded.length > 0 && !unbanChars.includes(encoded[0])){
                bias[encoded[0]] = -100
            }
            for(const _alt of charAlt){
                const encodedWithPrefix = nativeIds[index++]
                if(encodedWithPrefix.length > 0 && !unbanChars.includes(encodedWithPrefix[0])){
                    bias[encodedWithPrefix[0]] = -100
                }
                const encodedWithSuffix = nativeIds[index++]
                if(encodedWithSuffix.length > 0 && !unbanChars.includes(encodedWithSuffix[0])){
                    bias[encodedWithSuffix[0]] = -100
                }
            }
        }
    }
    else {
        await runJavaScript()
    }
    localStorage.setItem('strongBan_' + data, JSON.stringify(bias))
    return bias
}

export async function getCharToken(char?:character|null){
    let persistant = 0
    let dynamic = 0

    if(!char){
        const c = getCurrentCharacter()
        char = c
    }


    const counts = tokenCountBatch()
    const basicTokenize = async (data:string) => {
        data = data.replace(/{{char}}/g, char.name).replace(/<char>/g, char.name)
        return await counts.add(data)
    }

    persistant += await basicTokenize(char.desc)
    persistant += await basicTokenize(char.personality ?? '')
    persistant += await basicTokenize(char.scenario ?? '')
    for(const lore of char.globalLore){
        let cont = lore.content.split('\n').filter((line) => {
            if(line.startsWith('@@')){
                return false
            }
            if(line === ''){
                return false
            }
            return true
        }).join('\n')
        dynamic += await basicTokenize(cont)
    }
    // The first three texts are the persistent ones.
    for(const [index, rest] of (await counts.settle()).entries()){
        if(index < 3){
            persistant += rest
        }
        else{
            dynamic += rest
        }
    }

    return {persistant, dynamic}
}

export async function getChatToken(chat:Chat) {
    let persistant = 0

    const chatTokenizer = new ChatTokenizer(0, 'name')
    const chatf = chat.message.map((d) => {
        return {
            role: d.role === 'user' ? 'user' : 'assistant',
            content: d.data,
        } as OpenAIChat
    })
    persistant += await chatTokenizer.tokenizeChats(chatf)

    return persistant
}
