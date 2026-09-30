import { expect, it } from 'vitest'
import { shortcutModifier, shortcutModifierLabel } from './hotkeyModifier'

it('adds Command on Mac while preserving saved Ctrl bindings', () => {
    expect(shortcutModifier({ ctrlKey: false, metaKey: true }, 'macos')).toBe(
        true,
    )
    expect(shortcutModifier({ ctrlKey: true, metaKey: false }, 'macos')).toBe(
        true,
    )
    expect(shortcutModifier({ ctrlKey: false, metaKey: false }, 'macos')).toBe(
        false,
    )
    for (const os of ['windows', 'linux', 'android'] as const) {
        expect(shortcutModifier({ ctrlKey: false, metaKey: true }, os)).toBe(
            false,
        )
        expect(shortcutModifier({ ctrlKey: true, metaKey: false }, os)).toBe(
            true,
        )
    }
})

for (const os of ['macos', 'ios'] as const) {
    it(`supports both Command and Ctrl and labels them on ${os}`, () => {
        expect(shortcutModifier({ ctrlKey: false, metaKey: true }, os)).toBe(true)
        expect(shortcutModifier({ ctrlKey: true, metaKey: false }, os)).toBe(true)
        expect(shortcutModifier({ ctrlKey: false, metaKey: false }, os)).toBe(false)
        expect(shortcutModifierLabel(os)).toBe('Ctrl/⌘')
    })
}
it('labels non-Apple modifiers as Ctrl', () => {
    for (const os of ['windows', 'linux', 'android'] as const) {
        expect(shortcutModifierLabel(os)).toBe('Ctrl')
    }
})
