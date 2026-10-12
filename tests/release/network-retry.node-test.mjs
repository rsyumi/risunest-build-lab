import assert from "node:assert/strict";
import test from "node:test";
import { httpError, withNetworkRetry } from "../../scripts/release/retry.mjs";

test("transient requests stop after three attempts and preserve the final error", async () => {
  const waits = [];
  let calls = 0;
  const error = new TypeError("fetch failed", { cause: Object.assign(new Error("reset"), { code: "ECONNRESET" }) });
  await assert.rejects(withNetworkRetry(async () => { calls++; throw error; }, {
    wait: async ms => waits.push(ms),
  }), actual => actual === error);
  assert.equal(calls, 3);
  assert.deepEqual(waits, [10000, 20000]);
});

test("HTTP retries honor rate limits, dispose response bodies and bound long waits", async () => {
  for (const [status, headers, expected] of [
    [504, {}, [10000]],
    [503, { "retry-after": "45" }, [45000]],
    [429, {}, [60000]],
    [403, { "retry-after": "90" }, [90000]],
    [403, {}, null],
    [401, {}, null],
    [404, {}, null],
    [422, {}, null],
    [503, { "retry-after": "3600" }, null],
  ]) {
    const response = new Response("untrusted body", { status, headers });
    const error = await httpError(response, `http-${status}`);
    assert.equal(response.bodyUsed, true);
    const waits = [];
    let calls = 0;
    const operation = withNetworkRetry(async () => {
      if (++calls === 1) throw error;
      return "complete";
    }, { wait: async ms => waits.push(ms) });
    if (expected) {
      assert.equal(await operation, "complete");
      assert.equal(calls, 2);
      assert.deepEqual(waits, expected);
    } else {
      await assert.rejects(operation, actual => actual === error);
      assert.equal(calls, 1);
      assert.deepEqual(waits, []);
    }
  }
  const reset = Math.ceil(Date.now() / 1000) + 90;
  const error = await httpError(new Response(null, { status: 403, headers: {
    "x-ratelimit-remaining": "0", "x-ratelimit-reset": String(reset),
    "retry-after": new Date(reset * 1000).toUTCString(),
  } }), "rate limited");
  assert.ok(error.retryAfterMs >= 89000 && error.retryAfterMs <= 91000);
});

test("filesystem, validation, TLS and ordinary cancellation errors are not retried", async () => {
  for (const error of [
    Object.assign(new Error("disk full"), { code: "ENOSPC" }),
    Object.assign(new Error("certificate expired"), { code: "CERT_HAS_EXPIRED" }),
    new Error("checksum mismatch"), new SyntaxError("invalid JSON"), new DOMException("cancelled", "AbortError"),
  ]) {
    let calls = 0;
    await assert.rejects(withNetworkRetry(async () => { calls++; throw error; }, {
      wait: async () => assert.fail("must not wait"),
    }), actual => actual === error);
    assert.equal(calls, 1);
  }
});

test("each attempt receives a fresh timeout that also covers response consumption", async () => {
  const signals = [];
  const keepAlive = setInterval(() => {}, 1000);
  try {
    await assert.rejects(withNetworkRetry(signal => {
      signals.push(signal);
      return new Promise((resolve, reject) => signal.addEventListener("abort", () => {
        reject(new DOMException("body aborted", "AbortError"));
      }, { once: true }));
    }, { timeoutMs: 5, wait: async () => {} }), { name: "TimeoutError" });
  } finally {
    clearInterval(keepAlive);
  }
  assert.equal(signals.length, 3);
  assert.equal(new Set(signals).size, 3);
  assert.ok(signals.every(signal => signal.aborted));
});
