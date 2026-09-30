export class ScreenshotPreparationError extends Error {
    constructor(readonly reason: 'readiness' | 'resource', cause?: unknown) {
        super(cause instanceof Error ? cause.message : `Screenshot ${reason} failed`, { cause })
        this.name = 'ScreenshotPreparationError'
    }
}
