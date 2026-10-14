import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import { assetName } from "../../scripts/release/common.mjs";
import { loadReadmeCatalog, renderReadme, updateReadmeDownloads } from "../../scripts/release/readme.mjs";
import { catalogFixture, entryFixture } from "./fixtures.mjs";
import { createSigner } from "./signing.mjs";

const readmes = Object.fromEntries(["README.md", "README.ko.md"].map(path =>
  [path, readFileSync(new URL(`../../${path}`, import.meta.url), "utf8")]));
const locale = path => path === "README.md" ? "en" : "ko";

function catalogFor(app = "1.2.0", sync = "1.1.0") {
  const catalog = catalogFixture();
  catalog.products = { app: entryFixture("app", app), sync: entryFixture("sync", sync) };
  catalog.publicationTag = `app-v${app}`;
  return catalog;
}

function fixture() {
  const signer = createSigner();
  let sequence = 0;
  const id = () => String(++sequence).padStart(40, "0");
  const trees = new Map(), commits = new Map(), blobs = new Map();
  const saveTree = contents => {
    const sha = id();
    const entries = Object.entries(contents).map(([path, content]) => {
      const blob = id(); blobs.set(blob, content);
      return { path, mode: "100644", type: "blob", sha: blob };
    });
    trees.set(sha, { contents, tree: entries });
    return sha;
  };
  const saveCommit = (tree, parents) => {
    const sha = id(); commits.set(sha, { tree: { sha: tree }, parents }); return sha;
  };
  const initial = Object.fromEntries(Object.entries(readmes).map(([path, text]) =>
    [path, renderReadme(text, catalogFor("1.0.0", "1.0.0"), locale(path))]));
  initial["unrelated.txt"] = "keep this file";
  const state = { signer, writes: [], patches: 0, beforePatch: null,
    head: saveCommit(saveTree(initial), []), releases: new Map(),
    contents() { return trees.get(commits.get(state.head).tree.sha).contents; },
    advance(changes) {
      state.head = saveCommit(saveTree({ ...state.contents(), ...changes }), [state.head]);
    },
    publish(catalog) {
      state.catalog = catalog;
      state.bytes = Buffer.from(JSON.stringify(catalog));
      state.signature = Buffer.from(signer.sign(state.bytes));
      state.releases = new Map(Object.values(catalog.products).filter(Boolean).map(({ release }, index) =>
        [release.tag, { id: index + 1, tag_name: release.tag, draft: false, prerelease: false,
          assets: release.downloads.map(download => ({ browser_download_url: download.url,
            state: "uploaded", size: download.size })) }]));
      state.latest = state.releases.get(catalog.publicationTag);
    },
  };
  state.publish(catalogFor());
  const github = {
    async getLatest() { return state.latest; },
    async getByTag(tag) { return state.releases.get(tag); },
    async downloadAsset(release, name) {
      assert.equal(release.id, state.latest.id);
      if (name === "manifest.json") return state.bytes;
      assert.equal(name, "manifest.json.sig"); return state.signature;
    },
    async request(path, { method = "GET", body } = {}) {
      if (method !== "GET") state.writes.push({ path, method, body });
      if (method === "GET") {
        if (path === "/git/ref/heads/main") return { object: { sha: state.head } };
        const sha = path.split("/").at(-1);
        if (path.startsWith("/git/commits/")) return commits.get(sha);
        if (path.startsWith("/git/trees/")) return { tree: trees.get(sha).tree, truncated: false };
        if (path.startsWith("/git/blobs/")) return { encoding: "base64", content: Buffer.from(blobs.get(sha)).toString("base64") };
      }
      if (path === "/git/trees" && method === "POST") {
        assert(body.tree.every(entry => Object.hasOwn(readmes, entry.path)));
        return { sha: saveTree({ ...trees.get(body.base_tree).contents,
          ...Object.fromEntries(body.tree.map(entry => [entry.path, entry.content])) }) };
      }
      if (path === "/git/commits" && method === "POST") return { sha: saveCommit(body.tree, body.parents) };
      if (path === "/git/refs/heads/main" && method === "PATCH") {
        state.patches++;
        assert.equal(body.force, false);
        await state.beforePatch?.();
        if (state.denied) throw new Error("github-patch-403");
        if (commits.get(body.sha).parents[0] !== state.head) throw new Error("github-patch-422");
        state.head = body.sha;
        if (state.loseResponse) throw new Error("synthetic lost response");
        return { object: { sha: state.head } };
      }
      throw new Error(`Unexpected GitHub request: ${method} ${path}`);
    },
  };
  return { state, options: { github, publicKey: signer.key } };
}

test("renders independent product versions and manifest URLs in both languages without changing other content", () => {
  const catalog = catalogFor();
  for (const [path, contents] of Object.entries(readmes)) {
    const output = renderReadme(contents, catalog, locale(path));
    const strip = text => text.replace(/<!-- release-downloads:(app|sync):start -->[\s\S]*?<!-- release-downloads:\1:end -->/g, "BLOCK");
    assert.equal(strip(output), strip(contents));
    assert.match(output, /RisuNest v1\.2\.0/);
    assert.match(output, /RisuNest Sync v1\.1\.0/);
    assert.match(output, /app-v1\.2\.0\/app-desktop-windows-aarch64\.nsis/);
    assert.match(output, /sync-v1\.1\.0\/sync-raw-linux-x86_64\.tar\.gz/);
    assert.equal((output.match(/\/releases\/download\//g) ?? []).length, 22);
    assert.equal(renderReadme(output, catalog, locale(path)), output);
    const crlf = contents.replace(/\r?\n/g, "\r\n");
    assert.equal(renderReadme(crlf, catalog, locale(path)), output.replace(/\r?\n/g, "\r\n"));
  }
});

test("the current package names reproduce both README download sections exactly", () => {
  const appVersion = /\[RisuNest v(\d+\.\d+\.\d+)\]/.exec(readmes["README.md"])[1];
  const syncVersion = /\[RisuNest Sync v(\d+\.\d+\.\d+)\]/.exec(readmes["README.md"])[1];
  const catalog = catalogFor(appVersion, syncVersion);
  for (const { release } of Object.values(catalog.products)) {
    for (const download of release.downloads) {
      const previousUrl = download.url;
      download.url = `https://github.com/rsyumi/RisuNest/releases/download/${release.tag}/${assetName(download)}`;
      download.signatureUrl = `${download.url}.sig`;
      for (const platform of Object.values(release.platforms)) if (platform.url === previousUrl) platform.url = download.url;
    }
  }
  for (const [path, text] of Object.entries(readmes)) assert.equal(renderReadme(text, catalog, locale(path)), text);
});

test("missing, repeated, reversed and overlapping markers fail without replacing arbitrary text", () => {
  const text = readmes["README.md"];
  for (const broken of [
    text.replace("<!-- release-downloads:app:start -->", ""),
    text + "\n<!-- release-downloads:sync:end -->",
    text.replace("<!-- release-downloads:app:start -->", "<!-- release-downloads:app:end -->")
      .replace("<!-- release-downloads:app:end -->\n\n-", "<!-- release-downloads:app:start -->\n\n-"),
    text.replace("<!-- release-downloads:app:end -->", "") + "\n<!-- release-downloads:app:end -->",
  ]) assert.throws(() => renderReadme(broken, catalogFor(), "en"), /readme-markers/);
});

test("invalid signatures, catalog identities, unpublished products and missing assets prevent branch writes", async () => {
  const corruptions = [
    state => { state.bytes = Buffer.from(state.bytes.toString().replace("1.2.0", "9.2.0")); },
    state => { state.latest.tag_name = "app-v9.0.0"; },
    state => { state.latest.draft = true; },
    state => { state.releases.get("sync-v1.1.0").prerelease = true; },
    state => { state.releases.delete("sync-v1.1.0"); },
    state => { state.latest.assets.pop(); },
    state => { state.latest.assets[0].size++; },
    state => { state.latest.assets[0].state = "new"; },
    state => { const catalog = catalogFor(); catalog.products.sync = null; state.publish(catalog); },
  ];
  for (const corrupt of corruptions) {
    const { state, options } = fixture();
    corrupt(state);
    await assert.rejects(updateReadmeDownloads(options), /signature|catalog-tag|readme-/);
    assert.equal(state.writes.length, 0);
  }
});

test("a valid signature does not make an incomplete download matrix acceptable", async () => {
  const { state, options } = fixture();
  state.catalog.products.app.release.downloads.pop();
  state.publish(state.catalog);
  await assert.rejects(loadReadmeCatalog(options.github, options.publicKey), /incomplete-downloads/);
});

test("both languages are validated before any Git object or branch write", async () => {
  const { state, options } = fixture();
  state.advance({ "README.ko.md": "No automatic sections here." });
  const head = state.head;
  await assert.rejects(updateReadmeDownloads(options), /invalid-readme-markers/);
  assert.equal(state.head, head);
  assert.equal(state.writes.length, 0);
});

test("commits both READMEs together, preserves other files and makes repeat runs a no-op", async () => {
  const { state, options } = fixture();
  const result = await updateReadmeDownloads(options);
  assert.deepEqual(result, { status: "updated", commit: state.head });
  assert.equal(state.patches, 1);
  const treeWrite = state.writes.find(write => write.path === "/git/trees");
  assert.deepEqual(treeWrite.body.tree.map(entry => entry.path).sort(), ["README.ko.md", "README.md"]);
  assert.equal(state.contents()["unrelated.txt"], "keep this file");
  for (const path of Object.keys(readmes)) {
    assert.match(state.contents()[path], /RisuNest v1\.2\.0/);
    assert.match(state.contents()[path], /RisuNest Sync v1\.1\.0/);
  }
  const writeCount = state.writes.length;
  assert.equal((await updateReadmeDownloads(options)).status, "unchanged");
  assert.equal(state.writes.length, writeCount);
});

test("a concurrent main edit retries from the new tree and refreshes the published catalog", async () => {
  const { state, options } = fixture();
  state.beforePatch = () => {
    if (state.patches !== 1) return;
    state.advance({ "README.md": `${state.contents()["README.md"]}\nNew user documentation.\n`,
      "unrelated.txt": "concurrent content" });
    state.publish(catalogFor("1.3.0", "1.2.0"));
  };
  assert.equal((await updateReadmeDownloads(options)).status, "updated");
  assert.equal(state.patches, 2);
  assert.match(state.contents()["README.md"], /New user documentation\./);
  assert.equal(state.contents()["unrelated.txt"], "concurrent content");
  for (const path of Object.keys(readmes)) {
    assert.match(state.contents()[path], /RisuNest v1\.3\.0/);
    assert.match(state.contents()[path], /RisuNest Sync v1\.2\.0/);
  }
});

test("a lost update response is confirmed without creating another commit", async () => {
  const { state, options } = fixture();
  state.loseResponse = true;
  assert.deepEqual(await updateReadmeDownloads(options), { status: "updated", commit: state.head });
  assert.equal(state.patches, 1);
  assert.equal(state.writes.filter(write => write.path === "/git/commits").length, 1);
});

test("permission failures are not retried and persistent contention stops after three attempts", async () => {
  const denied = fixture();
  denied.state.denied = true;
  await assert.rejects(updateReadmeDownloads(denied.options), /github-patch-403/);
  assert.equal(denied.state.patches, 1);
  const busy = fixture();
  busy.state.beforePatch = () => busy.state.advance({ "unrelated.txt": `edit ${busy.state.patches}` });
  await assert.rejects(updateReadmeDownloads(busy.options), /readme-branch-kept-changing/);
  assert.equal(busy.state.patches, 3);
  assert.equal(busy.state.contents()["unrelated.txt"], "edit 3");
  assert.doesNotMatch(busy.state.contents()["README.md"], /RisuNest v1\.2\.0/);
});
