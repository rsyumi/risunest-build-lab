export interface TransferRateSample {
    id: string
    atMs: number
    sentBytes: string
    receivedBytes: string
    sending: boolean
    receiving: boolean
}

/** Bytes per second over the last few seconds of samples. */
export function createRateMeter(windowMs = 3000) {
    const samples: { at: number; bytes: number }[] = []
    return {
        add(at: number, bytes: number) {
            samples.push({ at, bytes })
            while (samples.length > 2 && at - samples[1].at >= windowMs) samples.shift()
        },
        rate(at?: number): number | undefined {
            if (samples.length < 2) return undefined
            const last = samples[samples.length - 1]
            const now = Math.max(last.at, at ?? last.at)
            let first = samples[0]
            if (at !== undefined) {
                if (now - last.at >= windowMs) return 0
                for (let i = 1; i < samples.length && samples[i].at <= now - windowMs; i++) first = samples[i]
            }
            return now > first.at ? Math.max(0, last.bytes - first.bytes) * 1000 / (now - first.at) : undefined
        },
        reset() { samples.length = 0 },
    }
}
