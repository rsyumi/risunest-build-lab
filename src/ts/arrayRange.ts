/**
 * `target.splice(start, deleteCount, ...items)` without passing `items` as
 * call arguments, which overflows the stack for long ranges.
 */
export function replaceArrayRange<T>(
    target: T[],
    start: number,
    deleteCount: number,
    items: readonly T[],
): T[] {
    const removed = target.slice(start, start + deleteCount)
    const tail = target.slice(start + deleteCount)
    target.length = start
    for (const item of items) target.push(item)
    for (const item of tail) target.push(item)
    return removed
}
