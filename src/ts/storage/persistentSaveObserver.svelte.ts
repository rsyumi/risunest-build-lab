import { untrack } from 'svelte'
import type { Database, character, groupChat } from './database.svelte'
import { PersistentMutationFencedError } from './saveCoordinator'

interface PersistentSaveObserverDependencies {
    readDatabase(): Database
    readSelectedCharacter(): character | groupChat | null
    markDirty(estimatedChangedBytes: number): void
}

function enumerableKeys(value: object | null | undefined): string[] {
    if (!value) return []
    // Track key additions/deletions without subscribing the parent effect to
    // values read by Svelte's property-descriptor trap during enumeration.
    return Reflect.ownKeys(value).filter((key): key is string =>
        typeof key === 'string' && untrack(() =>
            Object.prototype.propertyIsEnumerable.call(value, key),
        ),
    )
}

/** Subscribe to arbitrary compatibility mutations without copying their values. */
function subscribeDeep(value: unknown): void {
    if (value === null || typeof value !== 'object') return
    if (Array.isArray(value)) {
        for (let index = 0; index < value.length; index++) subscribeDeep(value[index])
        return
    }
    for (const key in value as Record<string, unknown>) {
        subscribeDeep((value as Record<string, unknown>)[key])
    }
}

export function observePersistentSaveChanges(
    dependencies: PersistentSaveObserverDependencies,
): () => void {
    // While a replacement installs its database the coordinator refuses the
    // mark and decides itself what an edit made meanwhile means. Throwing here
    // would stop the rest of the effects Svelte runs for that replacement.
    const markDirty = () => {
        try {
            dependencies.markDirty(0)
        } catch (error) {
            if (!(error instanceof PersistentMutationFencedError)) throw error
        }
    }
    return $effect.root(() => {
        $effect(() => {
            const database = dependencies.readDatabase()
            for (const key of enumerableKeys(database)) {
                if (key !== 'characters') {
                    $effect(() => {
                        subscribeDeep(database[key])
                        untrack(markDirty)
                    })
                }
            }
            // A deep observer knows that something changed, not the byte size
            // of that change. The complete root size would force an immediate
            // save for every keystroke whenever an unchanged payload is large.
            untrack(markDirty)
        })
        $effect(() => {
            const character = dependencies.readSelectedCharacter()
            if (character) {
                for (const key of enumerableKeys(character)) {
                    if (key !== 'chats') subscribeDeep(character[key])
                }
            }
            // Coordinator reads must not expand either observer's dependencies.
            untrack(markDirty)
        })
        $effect(() => {
            const chats = dependencies.readSelectedCharacter()?.chats ?? []
            // The parent observes collection identity/length. Per-slot effects
            // follow replacements and reorders without walking sibling history
            // when one conversation changes.
            for (let index = 0; index < chats.length; index++) {
                $effect(() => {
                    const chat = chats[index]
                    for (const key of enumerableKeys(chat)) {
                        if (key !== 'message') subscribeDeep(chat[key])
                    }
                    untrack(markDirty)
                })
                $effect(() => {
                    const chat = chats[index]
                    // Metadata-only selected shells deliberately expose a
                    // non-enumerable message getter which must not be invoked.
                    for (const key of enumerableKeys(chat)) {
                        if (key === 'message') {
                            const messages = chat.message
                            if (Array.isArray(messages)) {
                                for (let index = 0; index < messages.length; index++) {
                                    $effect(() => {
                                        subscribeDeep(messages[index])
                                        untrack(markDirty)
                                    })
                                }
                            } else subscribeDeep(messages)
                        }
                    }
                    untrack(markDirty)
                })
            }
            untrack(markDirty)
        })
    })
}
