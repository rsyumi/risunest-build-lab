import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import test from "node:test";
import { publishRelease } from "../../scripts/release/publication.mjs";
import { renderReadme } from "../../scripts/release/readme.mjs";
import { catalogFixture, productFixture, entryFixture } from "./fixtures.mjs";
import { createSigner } from "./signing.mjs";

function setup({ latest = true, product = "app", version = "1.1.0" } = {}) {
  const signer = createSigner();
  const release = productFixture(product, version);
  release.notes = "# Synthetic release\n\n## English\n\n- Improved downloads.\n\n## 한국어\n\n- 다운로드 개선.";
  release.localizedNotes = { en: "- Improved downloads.", ko: "- 다운로드 개선." };
  const productBytes = Buffer.from(JSON.stringify(release));
  const old = catalogFixture();
  const oldRelease = { id: 1, tag_name: old.publicationTag, draft: false, prerelease: false, target_commitish: "a".repeat(40) };
  const nextRelease = { id: 2, tag_name: release.tag, draft: true, prerelease: false, target_commitish: release.sourceCommit, body: release.notes };
  const assets = new Map();
  if (latest) {
    const bytes = Buffer.from(JSON.stringify(old));
    assets.set("1/manifest.json", bytes);
    assets.set("1/manifest.json.sig", Buffer.from(signer.sign(bytes)));
  }
  const events = [];
  const github = {
    latest: latest ? oldRelease : null,
    async getByTag(tag) { return [nextRelease, oldRelease].find(release => release.tag_name === tag && !release.draft) ?? null; },
    async getById(id) { return [nextRelease, oldRelease].find(release => release.id === id) ?? null; },
    async getLatest() { return this.latest; },
    async assertTagCommit() {},
    async hasPublishedStable() { return false; },
    async listPublishedStable() { return [oldRelease, nextRelease].filter(release => !release.draft); },
    async downloadAsset(r, name) { const bytes = assets.get(`${r.id}/${name}`); if (!bytes) throw new Error("missing-asset"); return bytes; },
    async uploadAsset(r, name, bytes) { events.push(name); assets.set(`${r.id}/${name}`, bytes); },
    async publish(_id, body) { events.push("publish"); nextRelease.body = body; nextRelease.draft = false; this.latest = nextRelease; },
    async makeLatest() { events.push("make-latest"); this.latest = nextRelease; },
  };
  const options = { github, releaseId: nextRelease.id, productBytes, productSignature: signer.sign(productBytes), publicKey: signer.key,
    expectedProduct: product, expectedTag: release.tag, expectedCommit: release.sourceCommit,
    publishedAt: "2026-09-16T00:00:00Z", signCatalog: async bytes => signer.sign(bytes),
    verifyAssets: async () => { events.push("verify-assets"); } };
  return { options, events, assets, nextRelease, signer, release };
}

for (const product of ["app", "sync"]) test(`${product} publication appends the README download table after verified notes`, async () => {
  const t = setup({ product });
  assert.equal((await publishRelease(t.options)).status, "published");
  assert.equal(t.events[0], "verify-assets");
  assert.equal(t.events.at(-1), "publish");
  const catalog = JSON.parse(t.assets.get("2/manifest.json"));
  const other = product === "app" ? "sync" : "app";
  assert.deepEqual(catalog.products[other], catalogFixture().products[other]);
  assert.equal(catalog.products[product].manifestSha256, createHash("sha256").update(t.options.productBytes).digest("hex"));
  assert.deepEqual(catalog.products[product].release, t.release);
  assert.deepEqual(t.assets.get("2/product-manifest.json"), t.options.productBytes);
  const template = ["app", "sync"].map(name =>
    `<!-- release-downloads:${name}:start -->\n<!-- release-downloads:${name}:end -->`).join("\n");
  const readme = renderReadme(template, catalog, "en");
  const section = readme.split(`<!-- release-downloads:${product}:start -->`)[1]
    .split(`<!-- release-downloads:${product}:end -->`)[0];
  const table = section.trim().split("\n").filter(line => line.startsWith("|")).join("\n");
  assert.ok(table.startsWith("| Platform | aarch64 (arm64) | x86_64 (x64) |"));
  assert.equal(t.nextRelease.body, `${t.release.notes}\n\n## Downloads / 다운로드\n\n${table}`);
  const body = t.nextRelease.body;
  assert.equal((await publishRelease(t.options)).status, "already-published");
  assert.equal(t.nextRelease.body, body);
});

test("missing or corrupted old catalog never becomes a bootstrap", async () => {
  for (const bytes of [undefined, Buffer.from("{}")]) {
    const t = setup();
    t.options.bootstrap = true;
    bytes ? t.assets.set("1/manifest.json", bytes) : t.assets.delete("1/manifest.json");
    await assert.rejects(publishRelease(t.options));
    assert.deepEqual(t.events, []);
  }
});

test("first publication requires explicit bootstrap and no existing stable releases", async () => {
  const t = setup({ latest: false });
  await assert.rejects(publishRelease(t.options), /bootstrap/);
  t.options.bootstrap = true;
  t.options.github.hasPublishedStable = async () => true;
  await assert.rejects(publishRelease(t.options), /bootstrap/);
  t.options.github.hasPublishedStable = async () => false;
  await publishRelease(t.options);
  assert.equal(JSON.parse(t.assets.get("2/manifest.json")).products.sync, null);
});

test("failed assets, changed source or invalid new signature cannot publish", async () => {
  for (const mutate of [
    t => t.options.verifyAssets = async () => { throw new Error("invalid-artifact"); },
    t => t.options.expectedCommit = "b".repeat(40),
    t => t.options.productSignature = t.options.productSignature.replace("timestamp:1", "timestamp:2"),
  ]) {
    const t = setup(); mutate(t);
    await assert.rejects(publishRelease(t.options));
    assert.equal(t.nextRelease.draft, true);
    assert.equal(t.events.includes("publish"), false);
  }
});

test("lost publish response is resolved by reading actual state", async () => {
  const t = setup();
  const original = t.options.github.publish.bind(t.options.github);
  t.options.github.publish = async (...args) => { await original(...args); throw new Error("connection-lost"); };
  assert.equal((await publishRelease(t.options)).status, "published");
  assert.equal((await publishRelease(t.options)).status, "already-published");
  assert.equal(t.events.filter(event => event === "publish").length, 1);
  assert.equal(t.nextRelease.body.split("## Downloads / 다운로드").length, 2);
});

test("a lost latest pointer is repaired without rewriting immutable assets", async () => {
  const t = setup();
  const old = t.options.github.latest;
  await publishRelease(t.options);
  const events = t.events.length;
  t.options.github.latest = old;
  assert.equal((await publishRelease(t.options)).status, "recovered");
  assert.deepEqual(t.events.slice(events), ["make-latest"]);
});

test("retrying an older publication preserves a later other-product catalog", async () => {
  const t = setup();
  await publishRelease(t.options);
  const later = JSON.parse(t.assets.get("2/manifest.json"));
  later.products.sync = entryFixture("sync", "1.2.0");
  later.publicationTag = "sync-v1.2.0";
  later.publishedAt = "2026-09-17T00:00:00Z";
  const bytes = Buffer.from(JSON.stringify(later));
  t.assets.set("3/manifest.json", bytes);
  t.assets.set("3/manifest.json.sig", Buffer.from(t.signer.sign(bytes)));
  t.options.github.latest = { id: 3, tag_name: later.publicationTag, draft: false, prerelease: false };
  const events = t.events.length;
  assert.equal((await publishRelease(t.options)).status, "already-published");
  assert.deepEqual(t.events.slice(events), []);
  assert.equal(t.options.github.latest.id, 3);
});

test("a missing latest pointer is recovered only after comparing every published catalog", async () => {
  const t = setup();
  await publishRelease(t.options);
  t.options.github.latest = null;
  t.options.github.hasPublishedStable = async () => true;
  const list = t.options.github.listPublishedStable;
  t.options.github.listPublishedStable = async () => [
    ...await list(), { id: 9, tag_name: "documentation-snapshot", draft: false, prerelease: false },
  ];
  const events = t.events.length;
  assert.equal((await publishRelease(t.options)).status, "recovered");
  assert.deepEqual(t.events.slice(events), ["make-latest"]);
  t.options.github.latest = null;
  t.assets.delete("1/manifest.json.sig");
  await assert.rejects(publishRelease(t.options), /missing-asset/);
  assert.equal(t.options.github.latest, null);
});

test("moving a tag after artifact upload prevents final publication", async () => {
  const t = setup();
  let checks = 0;
  t.options.github.assertTagCommit = async () => { if (++checks > 1) throw new Error("tag-commit-mismatch"); };
  await assert.rejects(publishRelease(t.options), /tag-commit-mismatch/);
  assert.equal(t.nextRelease.draft, true);
  assert.equal(t.events.includes("publish"), false);
});

test("publication requires the prepared release ID and rejects identity changes", async () => {
  for (const mutate of [t => delete t.options.releaseId, t => t.options.releaseId = 9,
    t => t.nextRelease.tag_name = "app-v9.0.0", t => t.nextRelease.prerelease = true]) {
    const t = setup(); mutate(t);
    await assert.rejects(publishRelease(t.options), /publication-release/);
    assert.deepEqual(t.events, []);
  }
});

test("draft identity is rechecked before publication and another published tag owner is rejected", async () => {
  for (const mutate of [t => t.nextRelease.prerelease = true,
    t => t.options.github.getByTag = async () => ({ id: 99, draft: false })]) {
    const t = setup();
    t.options.verifyAssets = async () => mutate(t);
    await assert.rejects(publishRelease(t.options), /draft-changed-before-publish|publication-release-mismatch/);
    assert.equal(t.events.includes("publish"), false);
  }
});
