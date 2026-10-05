import {
    allRepairSelection,
    dataHealthDeepFraction,
    groupDataHealthFindings,
    isDataHealthCancellation,
    preferredRepairSelection,
    toggleRepairSelection,
    type DataHealthFinding,
    type DataHealthGroup,
    type DataHealthResult,
    type RepairCandidate,
    type RepairJournalSummary,
    type RepairPreview,
} from './dataHealth'

export type DataHealthRun = 'quick' | 'deep' | null

export interface DataHealthSnapshot {
    loading: boolean
    running: DataHealthRun
    /** A deep scan that stopped before it finished, so resuming is offered. */
    resumable: boolean
    result: DataHealthResult | null
    groups: DataHealthGroup[]
    deepFraction: number | null
    failure: 'load' | 'scan' | 'repair' | 'undo' | 'refresh' | 'preview' | 'discard' | 'complete' | null
    applied: { remaining: number } | null
    activity: 'quick' | 'deep' | 'repair' | 'undo' | 'preview' | 'load' | 'discard' | 'complete' | null
    failed: boolean
    /** What the diagnosis can be answered with, and what the reader has chosen. */
    candidates: RepairCandidate[]
    selection: string[]
    preview: RepairPreview | null
    /** Repairs that can still be undone, newest first. */
    journals: RepairJournalSummary[]
    /** Records an undo left alone because they changed after the repair. */
    skipped: string[]
    repairing: boolean
}

export interface DataHealthDependencies {
    getResult(): Promise<DataHealthResult | null>
    scan(): Promise<DataHealthResult>
    deepScan(resume: boolean): Promise<DataHealthResult>
    cancel(): Promise<void>
    planRepair(): Promise<RepairCandidate[]>
    previewRepair(selection: string[], expectedScannedAt: number): Promise<RepairPreview>
    applyRepair(
        selection: string[],
        snapshot: boolean,
        expectedRevision: number,
        expectedScannedAt: number,
    ): Promise<{ result: DataHealthResult }>
    discardIntent(finding: number, expectedRevision: number, expectedScannedAt: number): Promise<{ result: DataHealthResult }>
    completeIntent(finding: number, expectedRevision: number, expectedScannedAt: number): Promise<{ result: DataHealthResult }>
    listJournals(): Promise<RepairJournalSummary[]>
    undoRepair(
        journalId: string,
        expectedRevision: number,
    ): Promise<{ result: DataHealthResult; skipped: string[] }>
}

function derive(
    result: DataHealthResult | null,
): Pick<DataHealthSnapshot, 'result' | 'groups' | 'deepFraction' | 'resumable'> {
    return {
        result,
        groups: result ? groupDataHealthFindings(result.items) : [],
        deepFraction: dataHealthDeepFraction(result),
        resumable: Boolean(
            result && result.depth === 'deep' && result.deep && !result.deep.complete,
        ),
    }
}

/**
 * Drives the diagnosis screen. A deep scan is a loop of bounded native pages, so a cancel takes
 * effect within one page and the progress it reports is what the screen shows.
 */
export function createDataHealthModel(deps: DataHealthDependencies) {
    let state: DataHealthSnapshot = {
        loading: false,
        running: null,
        failed: false,
        failure: null,
        applied: null,
        activity: null,
        candidates: [],
        selection: [],
        preview: null,
        journals: [],
        skipped: [],
        repairing: false,
        ...derive(null),
    }
    const listeners = new Set<(snapshot: DataHealthSnapshot) => void>()
    const update = (next: Partial<DataHealthSnapshot>) => {
        state = { ...state, ...next, ...("failure" in next ? { failed: next.failure !== null } : {}) }
        listeners.forEach((listener) => listener(state))
    }
    const mutationFailed = (error: unknown, action: 'repair' | 'undo' | 'complete') => {
        const code = error && typeof error === 'object' && 'code' in error ? error.code : null
        if (code === 'committed' || code === 'activation-committed-refresh-failed') {
            update({ failure: 'refresh', candidates: [], selection: [], preview: null, journals: [], ...derive(null) })
        } else update({ failure: action })
    }
    let cancelRequested = false

    const finish = (result: DataHealthResult | null) =>
        update({ ...derive(result), candidates: [], selection: [], preview: null })

    /** A scan replaces the diagnosis, so the repair choices are read again for the new one. */
    const finishScan = async (result: DataHealthResult): Promise<void> => {
        finish(result)
        try {
            await loadPlan()
        } catch {
            update({ failure: 'preview' })
        }
    }

    /** Reads what the current diagnosis can be answered with, and preselects the fixed choices. */
    const loadPlan = async (): Promise<void> => {
        if (!state.result || state.result.items.length === 0) return
        const candidates = await deps.planRepair()
        const selection = preferredRepairSelection(candidates)
        update({ candidates, selection })
        await refreshPreview(selection)
    }

    const refreshPreview = async (selection: string[]): Promise<void> => {
        update({
            preview:
                selection.length > 0 && state.result
                    ? await deps.previewRepair(selection, state.result.scannedAt) : null,
        })
    }

    const runDeep = async (resume: boolean): Promise<void> => {
        let next = resume
        for (;;) {
            const result = await deps.deepScan(next)
            if (result.deep?.complete || cancelRequested) {
                await finishScan(result)
                return
            }
            finish(result)
            next = true
        }
    }

    const start = async (
        running: Exclude<DataHealthRun, null>,
        action: () => Promise<void>,
    ): Promise<void> => {
        if (state.activity || state.loading) return
        cancelRequested = false
        update({ running, activity: running, failure: null, applied: null })
        try {
            await action()
        } catch (error) {
            if (!isDataHealthCancellation(error)) {
                update({ failure: 'scan' })
                throw error
            }
        } finally {
            update({ running: null, activity: null })
        }
    }

    return {
        snapshot: () => state,
        subscribe(listener: (snapshot: DataHealthSnapshot) => void) {
            listeners.add(listener)
            listener(state)
            return () => listeners.delete(listener)
        },
        /** Shows the last diagnosis without scanning again. */
        async load(): Promise<void> {
            if (state.activity || state.loading) return
            update({ loading: true, activity: "load", failure: null })
            try {
                finish(await deps.getResult())
            } catch {
                update({ failure: "load" })
            } finally {
                update({ loading: false, activity: null })
            }
        },
        quickScan(): Promise<void> {
            return start('quick', async () => finishScan(await deps.scan()))
        },
        deepScan(resume: boolean): Promise<void> {
            return start('deep', () => runDeep(resume))
        },
        async cancel(): Promise<void> {
            if (!state.running) return
            cancelRequested = true
            await deps.cancel()
        },
        /** Reads the repair choices for the diagnosis on screen. */
        async loadRepairs(): Promise<void> {
            if (state.activity || state.loading) return
            update({ activity: "load" })
            try {
                await loadPlan()
                update({ journals: await deps.listJournals() })
            } catch {
                update({ failure: "load" })
            } finally { update({ activity: null }) }
        },
        /** One choice per finding: picking one drops the other answers to the same finding. */
        async toggle(id: string): Promise<void> {
            if (state.activity || state.loading) return
            update({ activity: "preview", failure: null })
            const selection = toggleRepairSelection(
                state.candidates,
                state.selection,
                id,
            )
            update({ selection })
            try {
                await refreshPreview(selection)
            } catch {
                update({ failure: "preview" })
            } finally { update({ activity: null }) }
        },
        /** Chooses one answer for every finding, or clears the selection. */
        async setAll(on: boolean): Promise<void> {
            if (state.activity || state.loading) return
            update({ activity: "preview", failure: null })
            const selection = on ? allRepairSelection(state.candidates) : []
            update({ selection })
            try {
                await refreshPreview(selection)
            } catch {
                update({ failure: "preview" })
            } finally { update({ activity: null }) }
        },
        async apply(snapshot: boolean): Promise<void> {
            if (state.activity || state.loading || state.selection.length === 0 || !state.result) return
            update({ repairing: true, activity: 'repair', failure: null, applied: null, skipped: [] })
            try {
                let applied: { result: DataHealthResult }
                try {
                    applied = await deps.applyRepair([...state.selection], snapshot, state.result.revision, state.result.scannedAt)
                } catch (error) { mutationFailed(error, 'repair'); throw error }
                finish(applied.result)
                update({ applied: { remaining: Object.values(applied.result.counts).reduce((sum, count) => sum + count, 0) } })
                try {
                    await loadPlan()
                    update({ journals: await deps.listJournals() })
                } catch { update({ failure: 'refresh' }) }
            } finally { update({ repairing: false, activity: null }) }
        },
        async discard(item: DataHealthFinding, diagnosis: DataHealthResult): Promise<void> {
            if (state.activity || state.loading || !state.result || state.result.revision !== diagnosis.revision || state.result.scannedAt !== diagnosis.scannedAt) return
            const finding = state.result.items.findIndex(current => current.code === item.code && current.owner.kind === item.owner.kind && current.owner.id === item.owner.id && current.locator?.sourcePath === item.locator?.sourcePath && current.intentAction === 'discard')
            if (finding < 0 || item.code !== 'intent-quarantined' || item.owner.kind !== 'intent' || !item.locator || item.intentAction !== 'discard') return
            update({ repairing: true, activity: 'discard', failure: null, applied: null })
            try {
                const discarded = await deps.discardIntent(finding, diagnosis.revision, diagnosis.scannedAt)
                await finishScan(discarded.result)
            } catch (error) {
                update({ failure: 'discard' })
                throw error
            } finally { update({ repairing: false, activity: null }) }
        },
        async complete(item: DataHealthFinding, diagnosis: DataHealthResult): Promise<void> {
            if (state.activity || state.loading || !state.result || state.result.revision !== diagnosis.revision || state.result.scannedAt !== diagnosis.scannedAt) return
            const finding = state.result.items.findIndex(current => current.code === item.code && current.owner.kind === item.owner.kind && current.owner.id === item.owner.id && current.locator?.sourcePath === item.locator?.sourcePath && current.intentAction === 'complete')
            if (finding < 0 || item.code !== 'intent-quarantined' || item.owner.kind !== 'intent' || !item.locator || item.intentAction !== 'complete') return
            update({ repairing: true, activity: 'complete', failure: null, applied: null })
            try {
                let completed: { result: DataHealthResult }
                try { completed = await deps.completeIntent(finding, diagnosis.revision, diagnosis.scannedAt) }
                catch (error) { mutationFailed(error, 'complete'); throw error }
                await finishScan(completed.result)
            } finally { update({ repairing: false, activity: null }) }
        },
        async undo(journalId: string): Promise<void> {
            if (state.activity || state.loading || !state.result) return
            update({ repairing: true, activity: 'undo', failure: null, applied: null, skipped: [] })
            try {
                let undone: { result: DataHealthResult; skipped: string[] }
                try {
                    undone = await deps.undoRepair(journalId, state.result.revision)
                } catch (error) { mutationFailed(error, 'undo'); throw error }
                finish(undone.result)
                update({ applied: { remaining: Object.values(undone.result.counts).reduce((sum, count) => sum + count, 0) }, skipped: undone.skipped })
                try {
                    await loadPlan()
                    update({ journals: await deps.listJournals() })
                } catch { update({ failure: 'refresh' }) }
            } finally { update({ repairing: false, activity: null }) }
        },
    }
}
