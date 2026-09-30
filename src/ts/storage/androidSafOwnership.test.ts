import { readFileSync } from 'node:fs'
import { resolve } from 'node:path'
import { describe, expect, it } from 'vitest'

const activity = readFileSync(resolve(process.cwd(), 'src-tauri/gen/android/app/src/main/java/io/github/rsyumi/risunest/MainActivity.kt'), 'utf8')

describe('Android selected document route', () => {
    it('uses the unowned document copy branch and has no selected URI deletion', () => {
        const route = activity.slice(activity.indexOf('private fun onSafDestinationSelected('), activity.indexOf('private fun recoverSafDestination('))
        expect(route).toContain('copySafDestinationOnIo(')
        expect(route).toContain('createdDocument = false')
        expect(route).toContain('deletePartial = { false }')
        expect(route).not.toContain('contentResolver.delete(')
    })

    it('keeps interrupted selected documents and reports possible partial files', () => {
        const start = activity.indexOf('private fun recoverSafDestination(')
        const route = activity.slice(start, activity.indexOf('\n  private fun ', start + 1))
        expect(route).toContain('interruptedSafDestinationWarnings()')
        expect(route).not.toContain('contentResolver.delete(')
    })
})
