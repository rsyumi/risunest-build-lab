import { afterEach, describe, expect, it } from 'vitest'

import {
    discardEditorDraft,
    discardEditorDraftsExcept,
    forgetEditorPopup,
    keepEditorDraft,
    pendingEditorDrafts,
    takeEditorDraft,
    type ChatEditorDraft,
    type ChatEditorPopup,
} from './chatEditorDrafts'
import { openTextEditorPopup, textEditorPopup } from './gui/textEditorPopup.svelte'
import type { Message } from './storage/database.svelte'

function makeDraft(index: number, message: Message, popup?: ChatEditorPopup): ChatEditorDraft {
    return { kind: 'original', draft: `draft ${index}`, index, evidence: structuredClone(message), popup }
}

function makePopup(): ChatEditorPopup {
    return { request: { value: '', save: async () => true }, owner: { save: async () => true, input: () => {}, cancel: () => {} } }
}

describe('chat editor drafts', () => {
    afterEach(() => {
        discardEditorDraftsExcept(null, null)
        textEditorPopup.request = null
    })

    it('hands a draft only to the same index showing an equal message', () => {
        const message: Message = { role: 'char', data: 'Edited', chatId: 'message-1' }
        const draft = makeDraft(1, message)
        keepEditorDraft('character', 'chat', draft)

        expect(takeEditorDraft('character', 'chat', 2, message)).toBeNull()
        expect(takeEditorDraft('character', 'chat', 1, { ...message, data: 'Changed' })).toBeNull()
        expect(takeEditorDraft('character', 'other-chat', 1, message)).toBeNull()
        expect(takeEditorDraft('character', 'chat', 1, { ...message })).toBe(draft)
        expect(pendingEditorDrafts('character', 'chat')).toEqual([])
    })

    it('keeps conversations apart even when their ids contain the separator', () => {
        const message: Message = { role: 'char', data: 'Edited' }
        keepEditorDraft('a|b', 'c', makeDraft(0, message))

        expect(pendingEditorDrafts('a', 'b|c')).toEqual([])
        expect(pendingEditorDrafts('a|b', 'c')).toHaveLength(1)
    })

    it('forgets only the draft whose popup was closed', () => {
        const message: Message = { role: 'char', data: 'Edited' }
        const popup = makePopup()
        const kept = makeDraft(0, message)
        keepEditorDraft('character', 'chat', kept)
        keepEditorDraft('character', 'chat', makeDraft(1, message, popup))

        forgetEditorPopup(popup)

        expect(pendingEditorDrafts('character', 'chat')).toEqual([kept])
    })

    it('closes the popup of a discarded draft', () => {
        const message: Message = { role: 'char', data: 'Edited' }
        const popup = makePopup()
        const draft = makeDraft(0, message, popup)
        keepEditorDraft('character', 'chat', draft)
        openTextEditorPopup(popup.request)

        discardEditorDraft('character', 'chat', draft)

        expect(pendingEditorDrafts('character', 'chat')).toEqual([])
        expect(textEditorPopup.request).toBeNull()
    })

    it('drops the drafts of every other conversation and closes their popups', () => {
        const message: Message = { role: 'char', data: 'Edited' }
        const popup = makePopup()
        const current = makeDraft(0, message)
        keepEditorDraft('character', 'current-chat', current)
        keepEditorDraft('character', 'left-chat', makeDraft(0, message, popup))
        openTextEditorPopup(popup.request)

        discardEditorDraftsExcept('character', 'current-chat')

        expect(pendingEditorDrafts('character', 'current-chat')).toEqual([current])
        expect(pendingEditorDrafts('character', 'left-chat')).toEqual([])
        expect(textEditorPopup.request).toBeNull()

        discardEditorDraftsExcept('character', null)
        expect(pendingEditorDrafts('character', 'current-chat')).toEqual([])
    })
})
