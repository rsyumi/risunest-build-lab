import type { ServerSyncLane, ServerSyncLaneName } from './serverSyncProgress'

/** An idle native transport lane with zero counters. */
export function serverSyncLane(lane: ServerSyncLaneName, overrides: Partial<ServerSyncLane> = {}): ServerSyncLane {
    return {
        lane,
        active: false,
        sending: false,
        receiving: false,
        step: 'idle',
        listed: 0,
        itemsDone: 0,
        itemsTotal: 0,
        filesDone: 0,
        filesTotal: 0,
        bytesDone: 0,
        bytesTotal: 0,
        sentBytes: 0,
        receivedBytes: 0,
        backlogDone: 0,
        backlogLeft: 0,
        ...overrides,
    }
}
