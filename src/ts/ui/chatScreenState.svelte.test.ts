import { expect, it } from 'vitest'
import { ChatScreenStateStore } from './chatScreenState.svelte'

const first = { characterId: 'character', conversationId: 'first' }
const second = { characterId: 'character', conversationId: 'second' }

it('preserves independent composer and anchor updates for stable owners', () => {
    const store = new ChatScreenStateStore()
    const composer = store.getComposer(first)
    composer.text = 'Unsent draft'
    composer.translation = 'Paired translation'
    store.writeAnchor(first, { index: 120, messageId: 'message', relativeOffset: -18, latest: false })
    expect(store.getComposer({ ...first })).toBe(composer)
    expect(store.getComposer(second).text).toBe('')
    expect(store.readAnchor(first)).toEqual({ index: 120, messageId: 'message', relativeOffset: -18, latest: false })
    expect(composer.translation).toBe('Paired translation')
})

it('a late send clears only the exact submitted pair and never a new owner', () => {
    const store = new ChatScreenStateStore()
    const composer = store.getComposer(first)
    composer.text = 'Submitted'
    composer.translation = 'Translation'
    const submitted = store.captureComposer(first)
    store.getComposer(second).text = 'Submitted'
    composer.translation = 'New translation draft'
    expect(store.clearSubmittedComposer(submitted)).toBe(false)
    expect(composer.text).toBe('Submitted')
    expect(store.clearSubmittedComposer(store.captureComposer(first))).toBe(true)
    expect(composer.text).toBe('')
    expect(composer.translation).toBe('')
    expect(store.getComposer(second).text).toBe('Submitted')
})

it('bounds retained entries and rejects completion after eviction or removal', () => {
    const store = new ChatScreenStateStore(2)
    store.getComposer(first).text = 'Large draft'.repeat(10000)
    const evicted = store.captureComposer(first)
    store.getComposer(second).text = 'Second draft'
    store.getComposer({ characterId: 'third', conversationId: 'chat' })
    expect(store.matchesComposer(evicted)).toBe(false)
    expect(store.getComposer(second).text).toBe('Second draft')
    const removed = store.captureComposer(second)
    store.pruneConversations('character', new Set())
    expect(store.matchesComposer(removed)).toBe(false)
    expect(store.getComposer(second).text).toBe('')
    store.getComposer(first).text = 'Removed character draft'
    store.pruneCharacters(new Set(['third']))
    expect(store.getComposer(first).text).toBe('')
})
