import { expect } from 'vitest'
import type { NativeFileJobDependencies, NativeFileJobStatus } from './nativeFileJobs'
import type { PersistentRoot } from './persistentDataStore'
import { createStoreStub } from './persistentDataStore.testSupport'
import type { PersistentMutationToken } from './saveCoordinator'

/** A block restore job status in `state`; tests override `kind` and `phase` where they matter. */
export function status(
    state: NativeFileJobStatus['state'],
    result?: NativeFileJobStatus['result'],
): NativeFileJobStatus {
    return {
        jobId: 'job-1',
        kind: 'restore-block-risu-save',
        state,
        phase: state === 'succeeded' ? 'complete' : 'reading-source',
        progress: {
            completedBytes: state === 'queued' ? 0 : 128,
            totalBytes: 128,
            completedItems: 0,
        },
        result,
    }
}

/** A persistent runtime at `revision` whose pauses, fences and refreshes report through `options`. */
export function restoreRuntime(
    revision: number,
    options: {
        capture?: (reason: string) => void | Promise<void>
        refresh?: (revision: number) => void | Promise<void>
        markRefreshRequired?: (revision: number, error: unknown) => void
        acquire?: () => void | Promise<void>
        release?: () => void
        projection?: 'applied' | 'refresh-required'
        /** Revision captured after explicit restore preparation and user choices. */
        fencedRevision?: number
    } = {},
) {
    let captures = 0
    return {
        getStorageAuthorityEpoch: () => 2,
        withPausedPersistentWrites: async <T>(reason: string, operation: (token: PersistentMutationToken) => Promise<T>) => {
            await options.capture?.(reason)
            await options.acquire?.()
            try { return await operation({revision:options.fencedRevision ?? revision,mutationGeneration:1} as PersistentMutationToken) }
            finally { options.release?.() }
        },
        store: createStoreStub({readRoot:async()=>({revision,value:{} as PersistentRoot})}),
        setActivatedLibraryRecoveryLifecycle: () => {},
        beginActivatedLibraryGuard: () => ({complete(){},abortUnchanged:async()=>{}}),
        refreshActivatedLibraryUnderPause: async () => {
            await options.refresh?.((options.fencedRevision ?? revision) + 1)
            if (options.projection === 'refresh-required') throw new Error('Activated library projection is incomplete')
            return {kind:'committed' as const, revision:(options.fencedRevision ?? revision) + 1, projection:'applied' as const}
        },
        retryCommittedWorkingSetRefresh: async () => {
            await options.refresh?.(revision + 1)
            return {
                kind: 'committed' as const,
                revision: revision + 1,
                projection: options.projection ?? 'applied' as const,
            }
        },
        capturePersistentMutationToken: async (reason: string) => {
            await options.capture?.(reason)
            return {
                revision: captures++ > 0 ? options.fencedRevision ?? revision : revision,
                mutationGeneration: 1,
            }
        },
        markCommittedWorkingSetRefreshRequired: (
            committedRevision: number,
            error: unknown,
        ) => options.markRefreshRequired?.(committedRevision, error),
        acquireDestructiveReplacementFence: async (token: { revision: number; mutationGeneration: number }) => {
            await options.acquire?.()
            expect(token.revision).toBe(options.fencedRevision ?? revision)
            return {
                revision: token.revision,
                refreshCommittedWorkingSet: async (
                    committedRevision: number,
                ) => {
                    await options.refresh?.(committedRevision)
                    return {
                        kind: 'committed',
                        revision: committedRevision,
                        projection: options.projection ?? 'applied',
                    } as const
                },
                release: () => options.release?.(),
            }
        },
    }
}

type HostAnswer = unknown | ((args: Record<string, unknown> | undefined) => unknown)

/**
 * A native file job host. It starts one job, answers each status poll with the next scripted
 * status (the last one repeats), acknowledges finalize, cancel and forget, and records every call.
 * `answers` adds or overrides commands; any other command fails the operation.
 */
export function createNativeJobHost(statuses: readonly unknown[], answers: Record<string, HostAnswer> = {}) {
    const queue = [...statuses]
    const calls: Array<[command: string, args: Record<string, unknown> | undefined]> = []
    const invoke = async (command: string, args?: Record<string, unknown>) => {
        calls.push([command, args])
        if (Object.hasOwn(answers, command)) {
            const answer = answers[command]
            return typeof answer === 'function' ? answer(args) : answer
        }
        switch (command) {
            case 'native_file_job_start': return { jobId: 'job-1' }
            case 'native_file_job_status': return queue.length > 1 ? queue.shift() : queue[0]
            case 'native_file_job_finalize':
            case 'native_file_job_cancel': return 'requested'
            case 'native_file_job_forget': return true
        }
        throw new Error(`Unexpected command: ${command}`)
    }
    return {
        calls,
        /** The command names in call order. */
        commands: () => calls.map(([command]) => command),
        dependencies: { isTauri: () => true, invoke, wait: async () => undefined } satisfies NativeFileJobDependencies,
    }
}
