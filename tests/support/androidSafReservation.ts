import { vi } from 'vitest'
import {
    reserveAndroidSafDestination,
    type AndroidSafJavascriptBridge,
} from '../../src/ts/storage/androidSafBridge'

/** The native picker is a process boundary; reservation orchestration stays real. */
export function androidSafReservationFixture(
    onReserve: () => void = () => {},
    reserveDestination = reserveAndroidSafDestination,
) {
    const target = new EventTarget()
    const bridge = {
        copyExport: vi.fn(),
        copyReservedExport: vi.fn(),
        reserveExport: vi.fn((requestId: string) => {
            onReserve()
            target.dispatchEvent(new CustomEvent('risunest-android-saf-reserved', {detail: {requestId}}))
        }),
        bindExport: vi.fn(async () => true),
        releaseExport: vi.fn(async () => true),
        getExportStatus: vi.fn(async (): Promise<string | null> => null),
    } satisfies AndroidSafJavascriptBridge
    const reserve: typeof reserveAndroidSafDestination = (name, kind, signal) =>
        reserveDestination(name, kind, signal, {
            bridge,
            createRequestId: () => crypto.randomUUID(),
            addEventListener: (name, listener) => target.addEventListener(name, listener),
            removeEventListener: (name, listener) => target.removeEventListener(name, listener),
        })
    return {reserve, bridge,
        failRelease() {
            bridge.getExportStatus.mockImplementation(async () => JSON.stringify({
                requestId: bridge.reserveExport.mock.calls.at(-1)?.[0],
                state: 'cancelled', warningCodes: ['partial-destination-may-remain'],
            }))
        },
        select: (requestId: string) => target.dispatchEvent(new CustomEvent('risunest-android-saf-reserved', {detail: {requestId}}))}
}
