import type { RisuEnvironmentLabel } from './platform'

export type PlatformShape = {
    runtime: 'web' | 'tauri'
    os: 'windows' | 'macos' | 'linux' | 'android' | 'ios'
    /** The hosted risuai.xyz web build. */
    hosted?: boolean
    firefox?: boolean
    standalone?: boolean
}

const osNames: Record<PlatformShape['os'], string> = {
    windows: 'Windows', macos: 'macOS', linux: 'Linux', android: 'Android', ios: 'iOS',
}

/**
 * A platform module whose flags follow from one runtime and OS, as platform.ts derives them.
 * Pass a function to switch platforms between tests; every flag reads it on access.
 */
export function platformModule(shape: PlatformShape | (() => PlatformShape) = { runtime: 'web', os: 'windows' }) {
    const current = typeof shape === 'function' ? shape : () => shape
    const tauri = () => current().runtime === 'tauri'
    const ios = () => current().os === 'ios'
    const android = () => current().os === 'android'
    const mobile = () => android() || ios()
    const label = () => osNames[current().os]
    return {
        get isTauri() { return tauri() },
        get isWeb() { return !tauri() && !!current().hosted },
        get isAndroid() { return android() },
        get isMobile() { return mobile() },
        get isTauriIOS() { return tauri() && ios() },
        get isTauriAndroid() { return tauri() && android() },
        get isTauriMobile() { return tauri() && mobile() },
        get isTauriDesktop() { return tauri() && !mobile() },
        get isFirefox() { return !!current().firefox },
        get isInStandaloneMode() { return !!current().standalone },
        isIOS: () => ios(),
        getRisuEnvironmentLabel: (): RisuEnvironmentLabel => tauri() ? 'local' : 'web',
        getFallbackOSLabel: () => label(),
        getDetailedOSLabel: async () => label(),
    } satisfies typeof import('./platform')
}
