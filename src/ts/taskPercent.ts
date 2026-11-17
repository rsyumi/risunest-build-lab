/** The whole percent of `total` that `completed` reaches, or null while the total is unknown. */
export function measuredTaskPercent(completed: number, total?: number): number | null {
    return Number.isFinite(completed) && Number.isFinite(total) && total! > 0
        ? Math.max(0, Math.min(100, Math.round(completed * 100 / total!))) : null
}
