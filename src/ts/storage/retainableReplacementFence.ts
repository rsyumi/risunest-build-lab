import type { PersistentDestructiveReplacementFence } from './persistentDataRuntime'

/** A held fence that several owners can hold. The fence is released when the last hold is. */
export interface RetainableReplacementFence extends PersistentDestructiveReplacementFence {
    /** Another hold on the same fence. Throws once every hold has been released. */
    retain(): PersistentDestructiveReplacementFence
}

export function retainableReplacementFence(
    fence: PersistentDestructiveReplacementFence,
): RetainableReplacementFence {
    let holds = 0
    const hold = (): PersistentDestructiveReplacementFence => {
        holds += 1
        let released = false
        return {
            get revision() {
                return fence.revision
            },
            refreshCommittedWorkingSet(revision, options) {
                if (released) {
                    return Promise.reject(
                        new Error('Destructive persistent replacement fence was released'),
                    )
                }
                return fence.refreshCommittedWorkingSet(revision, options)
            },
            release() {
                if (released) return
                released = true
                holds -= 1
                if (holds === 0) fence.release()
            },
        }
    }
    const first = hold()
    return {
        get revision() {
            return first.revision
        },
        refreshCommittedWorkingSet: (revision, options) =>
            first.refreshCommittedWorkingSet(revision, options),
        release: () => first.release(),
        retain() {
            if (holds === 0) {
                throw new Error('Destructive persistent replacement fence was released')
            }
            return hold()
        },
    }
}
