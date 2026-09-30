import { platform } from '@tauri-apps/plugin-os'

/** Some WebKit versions report the IME confirmation key after compositionend. */
export function isCompositionKey(event: Pick<KeyboardEvent, 'isComposing' | 'keyCode'>): boolean {
    return event.isComposing || event.keyCode === 229
}

function nativePlatform() {
    return (globalThis as typeof globalThis & { __TAURI_INTERNALS__?: unknown })
        .__TAURI_INTERNALS__ ? platform() : undefined
}

export function acceptsCommand(os: string | undefined): boolean {
    return os === 'macos' || os === 'ios'
}

export function shortcutModifierLabel(os = nativePlatform()): string {
    return acceptsCommand(os) ? 'Ctrl/⌘' : 'Ctrl'
}

/** Keep existing Ctrl bindings usable and add the native Apple Command equivalent. */
export function shortcutModifier(
    event: Pick<KeyboardEvent, 'ctrlKey' | 'metaKey'>,
    os = nativePlatform(),
): boolean {
    return event.ctrlKey || (acceptsCommand(os) && event.metaKey)
}
