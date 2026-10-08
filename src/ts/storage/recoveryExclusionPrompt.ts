import { language } from 'src/lang'
import { alertActionConfirm } from '../alert'
import { getStartupExclusions, updateStartupExclusions } from './deviceSettings'
import {
    confirmRecoveryExclusions,
    RECOVERY_EXCLUSIONS,
    type RecoveryExclusion,
} from './recoveryMode.svelte'

export function exclusionName(exclusion: RecoveryExclusion): string {
    const strings = language.risuNest.recovery
    return {
        plugins: strings.excludePlugins,
        modules: strings.excludeModules,
        regex: strings.excludeRegex,
        theme: strings.excludeTheme,
        sync: strings.excludeSync,
        autoUpdate: strings.excludeAutoUpdate,
        account: strings.excludeAccount,
    }[exclusion]
}

function keepOnThisDevice(exclusions: RecoveryExclusion[]): void {
    const kept = new Set([...getStartupExclusions(), ...exclusions])
    updateStartupExclusions(RECOVERY_EXCLUSIONS.filter((item) => kept.has(item)))
}

/** Offers to keep what a finished start left off, and keeps it on this device only when confirmed. */
export function offerToKeepRecoveryExclusions(
    excluded: readonly RecoveryExclusion[],
    persist: (exclusions: RecoveryExclusion[]) => void = keepOnThisDevice,
): Promise<boolean> {
    const strings = language.risuNest.recovery
    return confirmRecoveryExclusions(
        excluded,
        (description) => alertActionConfirm({
            title: strings.keepTitle,
            description,
            actionLabel: strings.keepAction,
            cancelLabel: strings.excludeTitle,
        }),
        persist,
        exclusionName,
        strings.keepDescription,
    )
}
