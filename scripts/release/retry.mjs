import { setTimeout as sleep } from "node:timers/promises";

const retryStatuses = new Set([408, 429, 500, 502, 503, 504]);
const retryCodes = new Set([
  "ECONNRESET", "ECONNREFUSED", "ETIMEDOUT", "EAI_AGAIN",
  "UND_ERR_SOCKET", "UND_ERR_CONNECT_TIMEOUT", "UND_ERR_HEADERS_TIMEOUT", "UND_ERR_BODY_TIMEOUT",
]);

export async function httpError(response, message) {
  const retryAfter = response.headers.get("retry-after");
  const exhausted = response.headers.get("x-ratelimit-remaining") === "0";
  const rateLimited = response.status === 429 || (response.status === 403 && (retryAfter !== null || exhausted));
  let retryAfterMs = rateLimited ? 60000 : 0;
  if (retryAfter !== null) {
    const milliseconds = /^\d+(?:\.\d+)?$/.test(retryAfter)
      ? Number(retryAfter) * 1000 : Date.parse(retryAfter) - Date.now();
    if (Number.isFinite(milliseconds)) retryAfterMs = Math.max(retryAfterMs, milliseconds);
  }
  const reset = Number(response.headers.get("x-ratelimit-reset"));
  if (rateLimited && exhausted && Number.isFinite(reset))
    retryAfterMs = Math.max(retryAfterMs, reset * 1000 - Date.now());
  await response.body?.cancel().catch(() => {});
  return Object.assign(new Error(message), {
    retryable: retryStatuses.has(response.status) || rateLimited,
    retryAfterMs,
  });
}

function isTransient(error) {
  for (let cause = error, depth = 0; cause && depth < 5; cause = cause.cause, depth++) {
    if (cause.retryable || cause.name === "TimeoutError" || retryCodes.has(cause.code)) return true;
  }
  return false;
}

export async function withNetworkRetry(operation, { wait = sleep, timeoutMs = 60000 } = {}) {
  for (let attempt = 1; attempt <= 3; attempt++) {
    const signal = AbortSignal.timeout(timeoutMs);
    try {
      return await operation(signal);
    } catch (error) {
      const failure = error?.name === "AbortError" && signal.aborted ? signal.reason : error;
      const delay = Math.max(attempt * 10000, failure.retryAfterMs ?? 0);
      // Do not shorten a server cooldown that exceeds the retry budget.
      if (attempt === 3 || !isTransient(failure) || delay > 120000) throw failure;
      console.warn(`Network operation failed; retrying in ${delay / 1000}s (${attempt + 1}/3).`);
      await wait(delay);
    }
  }
}
