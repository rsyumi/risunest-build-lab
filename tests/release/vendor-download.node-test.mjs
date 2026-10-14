import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import { mkdtempSync, readFileSync, readdirSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { Readable } from "node:stream";
import test from "node:test";
import { download } from "../../scripts/release/vendor.mjs";

const bytes = Buffer.from("synthetic cloudflared bytes");
const sha256 = createHash("sha256").update(bytes).digest("hex");

test("vendor downloads discard partial files and publish only checksum-verified bytes", async t => {
  const root = mkdtempSync(join(tmpdir(), "risunest-vendor-retry-"));
  t.after(() => rmSync(root, { recursive: true, force: true }));
  const destination = join(root, "cloudflared");
  let calls = 0;
  const waits = [];
  await download("https://example.invalid/binary", destination, sha256, {
    fetcher: async () => {
      calls++;
      if (calls === 1) return new Response(null, { status: 504 });
      if (calls === 2) return new Response(Readable.toWeb(Readable.from((async function* () {
        yield Buffer.from("partial");
        throw Object.assign(new Error("connection reset"), { code: "ECONNRESET" });
      })())));
      return new Response(bytes);
    },
    wait: async ms => { waits.push(ms); assert.deepEqual(readdirSync(root), []); },
  });
  assert.equal(calls, 3);
  assert.equal(waits.length, 2);
  assert.deepEqual(readFileSync(destination), bytes);
  assert.deepEqual(readdirSync(root), ["cloudflared"]);
});

test("vendor failures clean owned temporary files without retrying validation or overwriting existing files", async t => {
  const root = mkdtempSync(join(tmpdir(), "risunest-vendor-failure-"));
  t.after(() => rmSync(root, { recursive: true, force: true }));
  const destination = join(root, "cloudflared");
  for (const [response, error] of [[() => new Response("wrong bytes"), /checksum mismatch/],
    [() => new Response(null, { status: 404 }), /404/]]) {
    let calls = 0;
    await assert.rejects(download("https://example.invalid/binary", destination, sha256, {
      fetcher: async () => { calls++; return response(); }, wait: async () => assert.fail("must not retry"),
    }), error);
    assert.equal(calls, 1);
    assert.deepEqual(readdirSync(root), []);
  }
  writeFileSync(destination, "existing file");
  await assert.rejects(download("https://example.invalid/binary", destination, sha256, {
    fetcher: async () => new Response(bytes), wait: async () => assert.fail("must not retry"),
  }), { code: "EEXIST" });
  assert.equal(readFileSync(destination, "utf8"), "existing file");
  assert.deepEqual(readdirSync(root), ["cloudflared"]);
});
