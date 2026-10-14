/** Admission only. The existing native file job manager owns execution/cancellation. */
let reservation: symbol | undefined;
let selections = 0;
const releasedListeners = new Set<() => void>();
const gates = new Set<() => Promise<void>>();

export function registerLibraryFileOperationGate(gate: () => Promise<void>): () => void {
  gates.add(gate);
  return () => { gates.delete(gate); };
}

export function waitForLibraryFileOperation(signal: AbortSignal): Promise<void> | undefined {
  signal.throwIfAborted();
  if (!gates.size) return;
  let abort!: () => void;
  const cancelled = new Promise<never>((_, reject) => {
    abort = () => reject(signal.reason ?? new DOMException("Operation cancelled", "AbortError"));
    signal.addEventListener("abort", abort, { once: true });
  });
  return Promise.race([Promise.all([...gates].map(gate => Promise.resolve().then(gate))), cancelled]).then(() => {
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
  return reservation !== undefined || selections > 0;
}

function notifyReleased(): void {
  if (isLibraryFileOperationReserved()) return;
  for (const listener of releasedListeners) {
    // A scheduling notification must not undo a settled file operation.
    try { listener(); } catch {}
  }
}

/** Keep the picker in the user gesture, including when it is nested in a file job. */
export async function withLibraryFileSelection<T>(select: () => Promise<T>): Promise<T> {
  selections++;
  try { return await select(); }
  finally { selections--; notifyReleased(); }
}

export function reserveLibraryFileOperation(): () => void {
  if (reservation) throw new Error("library-file-operation-busy");
  const token = Symbol("library-file-operation");
  reservation = token;
  return () => {
    if (reservation !== token) return;
    reservation = undefined;
    notifyReleased();
  };
}
