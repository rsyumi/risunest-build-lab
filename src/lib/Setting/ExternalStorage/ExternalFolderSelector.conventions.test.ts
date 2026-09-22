import { readFileSync } from 'node:fs'
import { describe, expect, it } from 'vitest'

describe('external folder selector source contracts', () => {
    it('keeps the mobile sheet and safe-area treatment', () => {
        const source = readFileSync('src/lib/Setting/ExternalStorage/ExternalFolderSelector.svelte', 'utf8')
        expect(source).toContain('@media (max-width: 40rem)')
        expect(source).toContain('env(safe-area-inset-bottom')
        expect(source).toContain('bg-black/60')
    })

    it('keeps raw provider identifiers out of the connection form', () => {
        const source = readFileSync('src/lib/Setting/ExternalStorage/ConnectionForm.svelte', 'utf8')
        expect(source).not.toMatch(/folderId|driveId|rootItemId/)
    })
})
