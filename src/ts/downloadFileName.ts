/** A download name is a basename on every supported platform. */
export function normalizeDownloadFileName(name: string): string {
    const cleaned = name.replace(/[<>:"/\\|?*\u0000-\u001f\u007f]/g, '_').replace(/[. ]+$/g, '')
    if (!cleaned || /^\.+$/.test(cleaned)) return 'export'
    return /^(con|prn|aux|nul|com[1-9]|lpt[1-9])(?:\.|$)/i.test(cleaned) ? `_${cleaned}` : cleaned
}
