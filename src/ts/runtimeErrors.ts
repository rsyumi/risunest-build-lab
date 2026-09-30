const MAX_ERROR_CHARS = 2048

function describeError(error: unknown): string {
    if (typeof error === 'string') return error.slice(0, MAX_ERROR_CHARS)
    if (error instanceof Error) {
        return `${error.name.slice(0, 100)}: ${error.message.slice(0, MAX_ERROR_CHARS)}`
            .slice(0, MAX_ERROR_CHARS)
    }
    return 'Unhandled non-Error value'
}

export function registerRuntimeErrorHandlers(
    target: Window,
    showError: (error: unknown) => void,
    recordError?: (message: string) => Promise<void>,
): () => void {
    const report = (kind: string, error: unknown) => {
        console.error(error)
        if (!recordError) return
        try {
            void recordError(`${kind}: ${describeError(error)}`.slice(0, MAX_ERROR_CHARS))
                .catch(() => {})
        } catch {
            // Reporting must not create another uncaught error.
        }
    }
    const errorHandler = (event: ErrorEvent) => {
        const error = event.error ?? (event.message || 'Unknown runtime error')
        report('Uncaught error', error)
        if (typeof Worker === 'undefined' || !(event.target instanceof Worker)) {
            showError(error)
        }
    }
    const rejectionHandler = (event: PromiseRejectionEvent) => {
        report('Unhandled rejection', event.reason)
        showError(event.reason)
    }
    target.addEventListener('error', errorHandler)
    target.addEventListener('unhandledrejection', rejectionHandler)
    return () => {
        target.removeEventListener('error', errorHandler)
        target.removeEventListener('unhandledrejection', rejectionHandler)
    }
}
