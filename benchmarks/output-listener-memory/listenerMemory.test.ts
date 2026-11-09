import 'fake-indexeddb/auto'
import { createHash, randomBytes } from 'node:crypto'
import { mkdir, readFile, writeFile } from 'node:fs/promises'
import { resolve } from 'node:path'
import { afterAll, describe, expect, it } from 'vitest'
import { createPinnedConversationPositionResolver } from '../../src/ts/plugins/pinnedConversationPosition'
import { dispatchChatOutputListeners, type ChatOutputListener } from '../../src/ts/plugins/pluginChatOutputListeners'
import { createProductionPluginChatOutputProjector, type PluginCompleteCharacter } from '../../src/ts/plugins/pluginDatabaseAccess'
import { createConversationSummaryStubFromChat } from '../../src/ts/storage/conversationResidency'
import type { Chat, Database, Message } from '../../src/ts/storage/database.svelte'
import { getPersistentDataStore, getRawPersistentDataStore } from '../../src/ts/storage/persistentDataStoreFactory'

const smoke = process.env.OUTPUT_LISTENER_SMOKE === '1'
const conversations = 10
const messagesPerConversation = smoke ? 20 : 2_600
const messageCharacters = 2_000
const outputs = 3
const variants = (process.env.OUTPUT_LISTENER_VARIANTS ?? 'full,current').split(',') as ('full' | 'current')[]
const files = ['src/ts/plugins/pluginChatOutputListeners.ts', 'src/ts/plugins/pluginDatabaseAccess.ts']
const gc = (globalThis as { gc?: () => void }).gc
const evidence: unknown[] = []

// Distinct flat ASCII strings, so stored and live copies cost what real message text costs.
const text = () => randomBytes(messageCharacters / 2).toString('hex')

function conversation(index: number): Chat {
    const message: Message[] = Array.from({ length: messagesPerConversation }, (_, row) => ({
        role: row % 2 ? 'char' : 'user', data: text(), chatId: `message-${index}-${row}`, time: 1_800_000_000_000 + row,
    }))
    return { id: `conversation-${index}`, name: `Synthetic conversation ${index}`, note: '', localLore: [], message }
}

function heapUsed() {
    return process.memoryUsage().heapUsed
}

function settledHeap() {
    gc?.()
    gc?.()
    return heapUsed()
}

describe('output listener event memory using real product modules', () => {
    it('records peak host heap during the listener phase for a large history and two listeners', async () => {
        const chats = Array.from({ length: conversations }, (_, index) => conversation(index))
        const database = { username: 'Synthetic', botPresets: [], characters: [{
            type: 'character', chaId: 'listener-owner', name: 'Synthetic owner', chats, chatPage: conversations - 1,
        }] } as unknown as Database
        const historyCharacters = chats.reduce((n, chat) => n + chat.message.reduce((m, message) => m + message.data.length, 0), 0)
        const raw = getRawPersistentDataStore()
        await raw.open()
        await raw.replaceFromDatabase(database)
        // The live character holds the current conversation and summary stubs, as after navigation.
        const current = chats.at(-1)!
        const liveCharacter = {
            ...database.characters[0],
            chats: chats.map((chat, index) => chat === current ? chat : createConversationSummaryStubFromChat('listener-owner', chat, index)),
        } as PluginCompleteCharacter
        chats.length = 0
        const resolvePosition = createPinnedConversationPositionResolver(() => ({
            store: getPersistentDataStore(), flushPendingData: async () => {},
        }))
        for (const variant of variants) {
            const projectScalable = createProductionPluginChatOutputProjector(structuredClone)
            const runs: unknown[] = []
            const initialHeapBytes = settledHeap()
            for (let output = 0; output < outputs; output++) {
                current.message.push({ role: 'char', data: text(), chatId: `output-${variant}-${output}` })
                const received: { conversations: number; messages: number; messageCharacters: number }[] = []
                const baselineHeapBytes = settledHeap()
                let peakHeapBytes = baselineHeapBytes
                const sample = () => { peakHeapBytes = Math.max(peakHeapBytes, heapUsed()) }
                const listener: ChatOutputListener = (event) => {
                    sample()
                    let messages = 0, characters = 0
                    for (const chat of [...event.char.chats, event.chat] as Chat[]) {
                        if (!Array.isArray(chat.message)) continue
                        messages += chat.message.length
                        for (const message of chat.message) characters += message.data.length
                    }
                    received.push({ conversations: event.char.chats.length, messages, messageCharacters: characters })
                }
                const sampler = setInterval(sample, 1)
                const start = performance.now()
                try {
                    await dispatchChatOutputListeners({
                        listeners: new Set([listener, (event) => listener(event)]), char: liveCharacter, chat: current,
                        resolvePosition, messageIndex: current.message.length - 1, windowStart: 0, fullHistory: variant === 'full',
                        projectScalable, onError: (error) => { throw error },
                    })
                } finally {
                    clearInterval(sampler)
                }
                const elapsedMs = performance.now() - start
                sample()
                expect(received).toHaveLength(2)
                runs.push({ output, elapsedMs, baselineHeapBytes, peakHeapBytes, peakDeltaBytes: peakHeapBytes - baselineHeapBytes,
                    retainedAfterOutputBytes: settledHeap() - initialHeapBytes, received })
            }
            evidence.push({ variant, listeners: 2, outputs: runs })
        }
        evidence.unshift({ fixture: { conversations, messagesPerConversation, messageCharacters, historyCharacters,
            historyUtf8Bytes: historyCharacters, currentConversation: current.id } })
    }, 900_000)
})

afterAll(async () => {
    const directory = resolve(process.env.OUTPUT_LISTENER_OUTPUT ?? '.tmp/output-listener-memory')
    await mkdir(directory, { recursive: true })
    const sourceRoot = process.env.OUTPUT_LISTENER_BASELINE_SOURCE ?? '.'
    const sourceHashes = Object.fromEntries(await Promise.all(files.map(async (file) =>
        [file, createHash('sha256').update(await readFile(resolve(sourceRoot, file))).digest('hex')])))
    const output = resolve(directory, `${process.env.OUTPUT_LISTENER_LABEL ?? 'run'}${smoke ? '-smoke' : ''}.json`)
    await writeFile(output, JSON.stringify({ schema: 1, timestamp: new Date().toISOString(), platform: process.platform, node: process.version,
        gcExposed: gc !== undefined, sourceHashes,
        scope: 'Host listener phase in Vitest with fake-indexeddb and the production projector; plugin iframe transfer and plugin-side copies are not measured.',
        evidence }, null, 2))
    process.stdout.write(`OUTPUT_LISTENER_MEMORY_ARTIFACT ${output}\n`)
})
