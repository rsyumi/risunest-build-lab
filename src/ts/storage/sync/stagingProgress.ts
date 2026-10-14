/** Current local staging work, independent of cumulative transfer counts. */
export interface StagingProgress {
    phase: 'read' | 'validate' | 'project' | 'copy' | 'check'
    done: number
    total: number | null
}

/** A phase counts passes and rows rather than items, so only its share is shown. */
export function stagingProgressView(progress: StagingProgress | null | undefined, text: Record<StagingProgress['phase'], string>) {
    if (!progress || !Object.hasOwn(text, progress.phase) || !Number.isSafeInteger(progress.done) || progress.done < 0
        || (progress.total !== null && (!Number.isSafeInteger(progress.total) || progress.total < 0))) return undefined
    const known = progress.total !== null && progress.total > 0
    return {
        label: text[progress.phase],
        fraction: known ? Math.min(progress.done, progress.total!) / progress.total! : null,
    }
}
