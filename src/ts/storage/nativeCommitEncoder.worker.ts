import { prepareNativePersistenceValue, UnsaveableValueError } from './nativePersistenceValue'
import type { CommitEnvelope } from './nativeCommitTransport'

self.onmessage = ({ data }: MessageEvent<CommitEnvelope>) => {
    try {
        const bytes = new TextEncoder().encode(JSON.stringify(prepareNativePersistenceValue(data)))
        self.postMessage({ bytes }, { transfer: [bytes.buffer] })
    } catch (error) {
        self.postMessage({
            error: error instanceof Error ? error.message : 'Persistence encoding failed',
            ...(error instanceof UnsaveableValueError ? {
                code: error.code, area: error.area, reason: error.reason, recordId: error.recordId,
            } : {}),
        })
    }
}
