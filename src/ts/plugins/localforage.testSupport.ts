import { vi } from 'vitest'

type InstanceOverrides = { [K in keyof LocalForage]?: unknown }

/**
 * A localforage instance backed by `values`. Without a map it keeps nothing: every read returns null.
 * Overrides replace single methods and must name real ones.
 */
export function localforageInstance(values?: Map<string, unknown>, overrides: InstanceOverrides = {}) {
    const instance = {
        getItem: vi.fn(async (key: string): Promise<any> => values?.get(key) ?? null),
        setItem: vi.fn(async (key: string, value: unknown): Promise<any> => {
            values?.set(key, value)
            return value
        }),
        removeItem: vi.fn(async (key: string) => { values?.delete(key) }),
        clear: vi.fn(async () => { values?.clear() }),
        keys: vi.fn(async () => [...values?.keys() ?? []]),
        length: vi.fn(async () => values?.size ?? 0),
        iterate: vi.fn(async (callback: (value: unknown, key: string, iteration: number) => unknown): Promise<any> => {
            let iteration = 0
            for (const [key, value] of values ?? []) {
                const result = callback(value, key, ++iteration)
                if (result !== undefined) return result
            }
        }),
    } satisfies Partial<LocalForage>
    return { ...instance, ...overrides }
}

/** The `localforage` module. Every instance it creates reads and writes the same `values`. */
export function localforageModule(values?: Map<string, unknown>, overrides: InstanceOverrides = {}) {
    const createInstance = () => localforageInstance(values, overrides)
    return { default: { createInstance: createInstance as unknown as LocalForage['createInstance'] } }
}
