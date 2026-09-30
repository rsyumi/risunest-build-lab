import { describe, expect, it } from 'vitest'

import {
    indexSideChatListRows,
    orderChatsByDroppedRows,
    orderFoldersByDroppedIds,
} from './sideChatListRows'

describe('side chat list row indexing', () => {
    it('keeps every chat once while preserving folder and chat order', () => {
        const folders = [
            { id: 'folder-b', name: 'B', folded: true },
            { id: 'folder-empty', name: 'Empty', folded: false },
            { id: '', name: 'Empty ID', folded: false },
            { id: 'folder-a', name: 'A', folded: false },
        ]
        const chat = (id: string, folderId: string | null) => ({
            id,
            folderId,
            message: [],
            note: '',
            name: id,
            localLore: [],
        })
        const chats = [
            chat('chat-a1', 'folder-a'),
            chat('chat-free', null),
            chat('chat-b1', 'folder-b'),
            chat('chat-orphan', 'missing-folder'),
            chat('chat-empty-id', ''),
            chat('chat-a2', 'folder-a'),
        ]

        const indexed = indexSideChatListRows(chats, folders)

        expect(
            indexed.folders.map((row) => ({
                id: row.folder.id,
                folded: row.folder.folded,
                chatIds: row.chats.map(({ chat }) => chat.id),
                indices: row.chats.map(({ index }) => index),
            })),
        ).toEqual([
            {
                id: 'folder-b',
                folded: true,
                chatIds: ['chat-b1'],
                indices: [2],
            },
            { id: 'folder-empty', folded: false, chatIds: [], indices: [] },
            { id: '', folded: false, chatIds: ['chat-empty-id'], indices: [4] },
            {
                id: 'folder-a',
                folded: false,
                chatIds: ['chat-a1', 'chat-a2'],
                indices: [0, 5],
            },
        ])
        expect(
            indexed.ungrouped.map(({ chat, index }) => ({
                id: chat.id,
                index,
            })),
        ).toEqual([
            { id: 'chat-free', index: 1 },
            { id: 'chat-orphan', index: 3 },
        ])
    })
})

describe('side chat list drop ordering', () => {
    const chat = (id: string, folderId?: string | null) => ({
        id,
        ...(folderId === undefined ? {} : { folderId }),
        message: [],
        note: '',
        name: id,
        localLore: [],
    })

    it('applies the dropped order and folder moves to the current chats', () => {
        const chats = [chat('a', 'folder-a'), chat('b'), chat('c', 'folder-b'), chat('d', 'folder-a')]

        const ordered = orderChatsByDroppedRows(chats, [
            { id: 'c', folderId: '' },
            { id: 'a' },
            { id: 'b', folderId: null },
            { id: 'd', folderId: null },
        ])

        expect(ordered).toEqual([chats[2], chats[0], chats[1], chats[3]])
        expect(ordered?.map((row) => row.folderId)).toEqual(['', 'folder-a', undefined, null])
    })

    it('refuses rows that no longer describe the current chats', () => {
        const chats = [chat('a', 'folder-a'), chat('b')]

        expect(orderChatsByDroppedRows(chats, [{ id: 'a', folderId: null }])).toBeNull()
        expect(orderChatsByDroppedRows(chats, [{ id: 'a', folderId: null }, { id: 'x' }])).toBeNull()
        expect(orderChatsByDroppedRows(chats, [{ id: 'a', folderId: null }, { id: 'a' }])).toBeNull()
        expect(chats[0].folderId).toBe('folder-a')
    })

    it('reorders folders only when the dropped ids match', () => {
        const folders = [
            { id: 'folder-a', name: 'A', folded: false },
            { id: 'folder-b', name: 'B', folded: true },
        ]

        expect(orderFoldersByDroppedIds(folders, ['folder-b', 'folder-a'])).toEqual([folders[1], folders[0]])
        expect(orderFoldersByDroppedIds(folders, ['folder-b'])).toBeNull()
        expect(orderFoldersByDroppedIds(folders, ['folder-b', 'folder-c'])).toBeNull()
    })
})
