import { afterEach, expect, it, vi } from 'vitest'
import { flushSync, mount, unmount } from 'svelte'
import { language } from 'src/lang'

vi.mock('src/ts/stores.svelte', () => {
    const DBState = $state({ db: {} })
    return { DBState }
})
vi.mock('@lucide/svelte', () => ({ TrashIcon: () => {}, XIcon: () => {} }))

import { DBState } from 'src/ts/stores.svelte'
import RealmBlockedCreators from './RealmBlockedCreators.svelte'
import type { Database } from 'src/ts/storage/database.svelte'

let instance: ReturnType<typeof mount> | undefined
afterEach(async () => {
    if (instance) await unmount(instance)
    instance = undefined
    document.body.replaceChildren()
})

it('unblocks one creator, refreshes the hub and shows the empty state after the last one', async () => {
    DBState.db = {
        blockedRealmCreators: [
            { id: 'creator-1', name: 'Creator One' },
            { id: 'creator-2', name: 'Creator Two' },
        ],
    } as unknown as Database
    const onUnblock = vi.fn()
    instance = mount(RealmBlockedCreators, { target: document.body, props: { onUnblock } })
    flushSync()

    const unblock = () => [...document.querySelectorAll('button')].filter((button) => button.textContent?.includes(language.unblockRealmCreator))
    unblock()[0].click()
    await vi.waitFor(() => expect(onUnblock).toHaveBeenCalledOnce())

    expect(DBState.db.blockedRealmCreators).toEqual([{ id: 'creator-2', name: 'Creator Two' }])
    flushSync()
    unblock()[0].click()
    await vi.waitFor(() => expect(document.body.textContent).toContain(language.noBlockedRealmCreators))
    expect(DBState.db.blockedRealmCreators).toEqual([])
})
