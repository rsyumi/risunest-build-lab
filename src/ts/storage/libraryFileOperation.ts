/** Admission only. The existing native file job manager owns execution/cancellation. */
let reservation: symbol | undefined;
const releasedListeners = new Set<() => void>();
let waitForSync: (() => Promise<void>) | undefined;

export function registerLibraryFileOperationGate(gate: () => Promise<void>): () => void {
  waitForSync = gate;
  return () => { if (waitForSync === gate) waitForSync = undefined; };
}

export function waitForLibraryFileOperation(signal: AbortSignal): Promise<void> | undefined {
  signal.throwIfAborted();
  if (!waitForSync) return;
  let abort!: () => void;
  const cancelled = new Promise<never>((_, reject) => {
    abort = () => reject(signal.reason ?? new DOMException("Operation cancelled", "AbortError"));
    signal.addEventListener("abort", abort, { once: true });
  });
  return Promise.race([Promise.resolve().then(waitForSync), cancelled]).then(() => {
    signal.throwIfAborted();
  }).finally(() => {
    signal.removeEventListener("abort", abort);
  });
}

export function subscribeLibraryFileOperationReleased(listener: () => void): () => void {
  releasedListeners.add(listener);
  return () => { releasedListeners.delete(listener); };
}

export function isLibraryFileOperationReserved(): boolean {
  return reservation !== undefined;
}

export function reserveLibraryFileOperation(): () => void {
  if (reservation) throw new Error("library-file-operation-busy");
  const token = Symbol("library-file-operation");
  reservation = token;
  return () => {
    if (reservation !== token) return;
    reservation = undefined;
    for (const listener of releasedListeners) {
      // A scheduling notification must not undo a settled file operation.
      try { listener(); } catch {}
    }
  };
}
