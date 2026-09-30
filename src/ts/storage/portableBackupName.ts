export function portableBackupSuggestedName(now = new Date()): string {
    return `risunest-${now.toISOString().replace(/[:.]/g, '-')}.risunest`
}
