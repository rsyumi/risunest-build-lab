export const APP_LOCK_KEY = 'risuNestAppLock'
export const APP_LOCK_TIMINGS = ['launch', 'immediate', 'after-1m', 'after-5m', 'after-15m', 'after-30m'] as const
export type AppLockTiming = typeof APP_LOCK_TIMINGS[number]

export interface PinRecord {
    algorithm: 'PBKDF2-SHA256'
    iterations: number
    salt: string
    hash: string
    [key: string]: unknown
}

export interface AppLockRecord {
    schema: 'risunest.app-lock/v1'
    enabled: boolean
    pin: PinRecord | null
    timing: AppLockTiming
    nativeUnlock: boolean
    hideRecentsPreview: boolean
    [key: string]: unknown
}

export function defaultAppLockRecord(): AppLockRecord {
    return {
        schema: 'risunest.app-lock/v1', enabled: false, pin: null,
        timing: 'launch', nativeUnlock: false, hideRecentsPreview: false,
    }
}

function bytesFromBase64(value: string): Uint8Array<ArrayBuffer> {
    const decoded = atob(value)
    return Uint8Array.from(decoded, (character) => character.charCodeAt(0))
}

function base64FromBytes(value: Uint8Array): string {
    return btoa(String.fromCharCode(...value))
}

function object(value: unknown): value is Record<string, unknown> {
    return typeof value === 'object' && value !== null && !Array.isArray(value)
}

export function readAppLockRecord(serialized: unknown): AppLockRecord {
    if (serialized === null) return defaultAppLockRecord()
    if (typeof serialized !== 'string') throw new Error('Unreadable app lock record')
    const value: unknown = JSON.parse(serialized)
    if (!object(value) || value.schema !== 'risunest.app-lock/v1'
        || typeof value.enabled !== 'boolean' || typeof value.nativeUnlock !== 'boolean'
        || typeof value.hideRecentsPreview !== 'boolean'
        || !APP_LOCK_TIMINGS.includes(value.timing as AppLockTiming)) {
        throw new Error('Invalid app lock record')
    }
    if (value.pin !== null) {
        const pin = value.pin
        if (!object(pin) || pin.algorithm !== 'PBKDF2-SHA256'
            || !Number.isSafeInteger(pin.iterations) || (pin.iterations as number) < 1
            || typeof pin.salt !== 'string' || typeof pin.hash !== 'string'
            || bytesFromBase64(pin.salt).length !== 16 || bytesFromBase64(pin.hash).length !== 32) {
            throw new Error('Invalid app PIN record')
        }
    } else if (value.enabled) {
        throw new Error('Enabled app lock requires a PIN')
    }
    return value as unknown as AppLockRecord
}

async function derivePin(pin: string, salt: Uint8Array<ArrayBuffer>, iterations: number): Promise<Uint8Array> {
    const key = await crypto.subtle.importKey('raw', new TextEncoder().encode(pin), 'PBKDF2', false, ['deriveBits'])
    return new Uint8Array(await crypto.subtle.deriveBits({
        name: 'PBKDF2', hash: 'SHA-256', salt, iterations,
    }, key, 256))
}

export async function createPinRecord(pin: string): Promise<PinRecord> {
    if (!/^\d{6}$/.test(pin)) throw new Error('App PIN must contain six digits')
    const salt = crypto.getRandomValues(new Uint8Array(16))
    const iterations = 100_000
    return {
        algorithm: 'PBKDF2-SHA256', iterations, salt: base64FromBytes(salt),
        hash: base64FromBytes(await derivePin(pin, salt, iterations)),
    }
}

export async function matchesPin(pin: string, stored: PinRecord): Promise<boolean> {
    if (!/^\d{6}$/.test(pin)) return false
    return base64FromBytes(await derivePin(pin, bytesFromBase64(stored.salt), stored.iterations)) === stored.hash
}

export function lockRequiredAfterDeparture(timing: AppLockTiming, departedAtMs: number, atMs: number): boolean {
    if (timing === 'launch') return false
    if (timing === 'immediate') return true
    if (!Number.isFinite(departedAtMs) || !Number.isFinite(atMs) || atMs < departedAtMs) return true
    const minutes = Number(timing.slice(6, -1))
    return atMs - departedAtMs >= minutes * 60_000
}
