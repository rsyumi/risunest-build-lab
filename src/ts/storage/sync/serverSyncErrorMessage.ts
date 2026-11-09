import { language } from 'src/lang'
import { isTauri } from '../../platform'
import { platform } from '@tauri-apps/plugin-os'
import { PREVIOUS_FILES_DOWNLOAD_FAILED } from './bindingFlow'

export const writerRecoveryCodes = ['writer-collision', 'equal-stamp-integrity']
export const registrationCodes = ['unauthorized', 'invalid-device-token', 'server-epoch-changed']
export const usedRegistrationCodes = ['registration-used', 'registration-integrity', 'registration-not-new']
export function serverSyncErrorMessage(error: unknown, onboarding = false, bound = true): string {
    const copy = language.risuNest.serverSync
    const token = typeof error === 'object' && error && 'code' in error ? String(error.code) : ''
    if (['clock-skew', 'incoming-clock-skew', 'accepted-clock-correction-required'].includes(token)) return language.lwwSync.clockBlocked
    if (token === 'writer-collision' && !bound) return language.lwwSync.writerCollisionNotConnected
    if (writerRecoveryCodes.includes(token)) return language.lwwSync.writerCollision
    if (token === 'unit-too-large') return language.lwwSync.unitTooLarge
    if (token === 'device-credential-unavailable') {
        let linux = false
        try { linux = isTauri && platform() === 'linux' } catch { /* The browser has no native platform. */ }
        return linux ? copy.credentialUnavailableLinux : copy.credentialUnavailable
    }
    if (registrationCodes.includes(token)) return language.lwwSync.registrationRevoked
    if (usedRegistrationCodes.includes(token)) return onboarding ? language.lwwSync.registrationUsedOnboarding : language.lwwSync.registrationUsed
    if (token === PREVIOUS_FILES_DOWNLOAD_FAILED) return language.lwwSync.downloadFailedNotConnected
    if (token === 'previous-storage-unavailable') return language.lwwSync.previousStorageUnavailable
    if (token.startsWith('qr-')) return token.includes('permission') ? copy.cameraDenied : copy.cameraUnavailable
    return copy.errorHelp
}
