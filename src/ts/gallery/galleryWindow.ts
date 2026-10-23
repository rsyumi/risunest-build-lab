export interface GalleryWindowInput {
    total: number
    width: number
    viewportHeight: number
    scrollOffset: number
    large: boolean
    mobile: boolean
    captions: boolean
}
export function galleryWindow(input: GalleryWindowInput) {
    const padding = input.mobile ? 8 : 12
    const gap = input.mobile ? 6 : 8
    const available = Math.max(1, input.width - padding * 2)
    const columns = input.mobile ? (input.large ? 2 : 3) : Math.max(1, Math.floor((available + gap) / ((input.large ? 240 : 168) + gap)))
    const cell = (available - gap * (columns - 1)) / columns
    const rowHeight = cell + (input.captions ? 22 : 0) + gap
    const rows = Math.ceil(input.total / columns)
    const firstRow = Math.min(rows, Math.max(0, Math.floor((input.scrollOffset - input.viewportHeight) / rowHeight)))
    const lastRow = Math.min(rows, Math.max(firstRow, Math.ceil((input.scrollOffset + input.viewportHeight * 2) / rowHeight)))
    return { columns, cell, gap, padding, rowHeight, firstRow, lastRow,
        offset: firstRow * columns, limit: Math.max(0, Math.min(input.total, lastRow * columns) - firstRow * columns),
        height: Math.max(0, rows * rowHeight - gap) + padding * 2 }
}
