import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import { assetName } from "../../scripts/release/common.mjs";
import { loadReadmeCatalog, parseIosConfiguration, renderAltSource, renderReadme, updateReadmeDownloads }
  from "../../scripts/release/readme.mjs";
import { catalogFixture, entryFixture } from "./fixtures.mjs";
import { createSigner } from "./signing.mjs";

const read = path => readFileSync(new URL(`../../${path}`, import.meta.url), "utf8");
const readmes = Object.fromEntries(["README.md", "README.ko.md"].map(path => [path, read(path)]));
const locale = path => path === "README.md" ? "en" : "ko";
const altSource = read("alt-source.json");
const managedFiles = [...Object.keys(readmes), "alt-source.json"];
const iosPaths = { plist: "src-tauri/gen/apple/risunest_iOS/Info.plist",
  entitlements: "src-tauri/gen/apple/risunest_iOS/risunest_iOS.entitlements",
  project: "src-tauri/gen/apple/project.yml" };
const iosSources = Object.fromEntries(Object.entries(iosPaths).map(([name, path]) => [name, read(path)]));
const ios = parseIosConfiguration(iosSources);

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
  const source = JSON.parse(altSource);
  source.apps[0].versions = [];
  initial["alt-source.json"] = renderAltSource(JSON.stringify(source), catalogFor("1.0.0", "1.0.0"), ios);
  initial["unrelated.txt"] = "keep this file";
  const state = { signer, writes: [], patches: 0, beforePatch: null,
    head: saveCommit(saveTree(initial), []), releases: new Map(),
    sources: new Map([["a".repeat(40), iosSources]]),
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
        const file = /^\/contents\/(.+)\?ref=([a-f0-9]{40})$/.exec(path);
        const name = file && Object.keys(iosPaths).find(key => iosPaths[key] === file[1]);
        const text = name && state.sources.get(file[2])?.[name];
        if (file) {
          if (text === undefined) throw new Error("github-get-404");
          return { type: "file", encoding: "base64", content: Buffer.from(text).toString("base64") };
        }
      }
      if (path === "/git/trees" && method === "POST") {
        assert(body.tree.every(entry => managedFiles.includes(entry.path)));
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

test("the iOS source retains version history, puts the newest app first and exposes legacy download fields", () => {
  const source = JSON.parse(altSource);
  source.apps[0].versions = [];
  const first = renderAltSource(JSON.stringify(source), catalogFor(), ios);
  const catalog = catalogFor("1.3.0");
  const release = catalog.products.app.release;
  release.localizedNotes.en = "New iOS release notes";
  release.pub_date = "2026-10-13T17:09:48.862Z";
  const newer = { entitlements: ["com.apple.developer.associated-domains"],
    privacy: { NSMicrophoneUsageDescription: "Record a message." }, minOSVersion: "17.0" };
  const updated = renderAltSource(first, catalog, newer);
  const app = JSON.parse(updated).apps[0];
  const ipa = release.downloads.find(download => download.os === "ios");
  assert.deepEqual(app.versions, [{ version: "1.3.0", buildVersion: "1.3.0", date: "2026-10-13T17:09:48Z",
    localizedDescription: "New iOS release notes", downloadURL: ipa.url, size: ipa.size, sha256: ipa.sha256,
    minOSVersion: "17.0" }, JSON.parse(first).apps[0].versions[0]]);
  assert.equal(JSON.parse(first).apps[0].versions[0].minOSVersion, ios.minOSVersion);
  assert.equal(app.version, "1.3.0");
  assert.equal(app.downloadURL, ipa.url);
  assert.equal(app.size, ipa.size);
  assert.equal(app.versionDate, "2026-10-13T17:09:48Z");
  assert.equal(app.versionDescription, "New iOS release notes");
  assert.deepEqual(app.appPermissions, { entitlements: newer.entitlements, privacy: newer.privacy });
  assert.equal(renderAltSource(updated, catalog, newer), updated);
  const sync = catalogFor("1.3.0", "1.2.0");
  sync.products.app = catalog.products.app;
  sync.publicationTag = "sync-v1.2.0";
  assert.equal(renderAltSource(updated, sync, newer), updated);
  const older = JSON.parse(renderAltSource(updated, catalogFor("1.1.0"), ios)).apps[0];
  assert.deepEqual(older.versions.map(item => item.version), ["1.3.0", "1.2.0", "1.1.0"]);
  assert.equal(older.versions[2].minOSVersion, ios.minOSVersion);
  assert.equal(older.downloadURL, ipa.url);
  assert.deepEqual(older.appPermissions, app.appPermissions);
  delete release.localizedNotes.en;
  assert.throws(() => renderAltSource(updated, catalog, newer), /alt-source-english-notes-missing/);
});

test("the shipped iOS configuration parses into the source bundle identity, permissions and minimum OS", () => {
  const app = JSON.parse(altSource).apps[0];
  assert.equal(app.bundleIdentifier, /PRODUCT_BUNDLE_IDENTIFIER: (\S+)/.exec(iosSources.project)[1]);
  assert.equal(ios.minOSVersion, JSON.parse(read("src-tauri/tauri.ios.conf.json")).bundle.iOS.minimumSystemVersion);
  const declared = Object.fromEntries([...iosSources.project.matchAll(/^\s+(NS\w+UsageDescription): (.+)$/gm)]
    .map(([, key, description]) => [key, description]));
  assert.deepEqual(ios.privacy, declared);
  const changed = parseIosConfiguration({ ...iosSources,
    plist: iosSources.plist.replace(/<\/dict>\s*<\/plist>/,
      "<key>NSMicrophoneUsageDescription</key>\n<string>Record &amp; send &lt;voice&gt;</string>\n</dict>\n</plist>"),
    project: iosSources.project.replace(/iOS: [\d.]+/, "iOS: \"17.0\"") });
  assert.equal(changed.privacy.NSMicrophoneUsageDescription, "Record & send <voice>");
  assert.equal(changed.minOSVersion, "17.0");
  assert.throws(() => parseIosConfiguration({ ...iosSources, project: "name: risunest\n" }), /alt-source-min-os-missing/);
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
    state => { state.sources.clear(); },
    state => { state.sources.set("a".repeat(40), { ...iosSources, project: "name: risunest\n" }); },
  ];
  for (const corrupt of corruptions) {
    const { state, options } = fixture();
    corrupt(state);
    await assert.rejects(updateReadmeDownloads(options), /signature|catalog-tag|readme-|github-get-404|alt-source-/);
    assert.equal(state.writes.length, 0);
  }
});

test("a valid signature does not make an incomplete download matrix acceptable", async () => {
  const { state, options } = fixture();
  state.catalog.products.app.release.downloads.pop();
  state.publish(state.catalog);
  await assert.rejects(loadReadmeCatalog(options.github, options.publicKey), /incomplete-downloads/);
});

test("both languages and the source are validated before any Git object or branch write", async () => {
  for (const changes of [{ "README.ko.md": "No automatic sections here." },
    { "alt-source.json": "invalid json" }, { "alt-source.json": '{"apps": []}' }]) {
    const { state, options } = fixture();
    state.advance(changes);
    const head = state.head;
    await assert.rejects(updateReadmeDownloads(options), /invalid-readme-markers|JSON|alt-source-app-missing/);
    assert.equal(state.head, head);
    assert.equal(state.writes.length, 0);
  }
});

test("commits both READMEs and the iOS source together, preserves other files and makes repeat runs a no-op", async () => {
  const { state, options } = fixture();
  const result = await updateReadmeDownloads(options);
  assert.deepEqual(result, { status: "updated", commit: state.head });
  assert.equal(state.patches, 1);
  const treeWrite = state.writes.find(write => write.path === "/git/trees");
  assert.deepEqual(treeWrite.body.tree.map(entry => entry.path).sort(), ["README.ko.md", "README.md", "alt-source.json"]);
  assert.equal(state.contents()["unrelated.txt"], "keep this file");
  const app = JSON.parse(state.contents()["alt-source.json"]).apps[0];
  assert.deepEqual(app.versions.map(item => item.version), ["1.2.0", "1.0.0"]);
  assert.equal(app.downloadURL, state.catalog.products.app.release.downloads.find(item => item.os === "ios").url);
  for (const path of Object.keys(readmes)) {
    assert.match(state.contents()[path], /RisuNest v1\.2\.0/);
    assert.match(state.contents()[path], /RisuNest Sync v1\.1\.0/);
  }
  const writeCount = state.writes.length;
  assert.equal((await updateReadmeDownloads(options)).status, "unchanged");
  assert.equal(state.writes.length, writeCount);
});

test("iOS permissions and minimum OS come from the published App source commit, not from main", async () => {
  const { state, options } = fixture();
  const catalog = catalogFor();
  const commit = "b".repeat(40);
  catalog.products.app.release.sourceCommit = commit;
  state.publish(catalog);
  state.sources = new Map([[commit, { ...iosSources,
    plist: iosSources.plist.replace(/<\/dict>\s*<\/plist>/,
      "<key>NSMicrophoneUsageDescription</key>\n<string>Record a message.</string>\n</dict>\n</plist>"),
    project: iosSources.project.replace(/iOS: [\d.]+/, "iOS: 17.0") }]]);
  assert.equal((await updateReadmeDownloads(options)).status, "updated");
  const app = JSON.parse(state.contents()["alt-source.json"]).apps[0];
  assert.deepEqual(app.appPermissions.privacy, { ...ios.privacy, NSMicrophoneUsageDescription: "Record a message." });
  assert.deepEqual(app.versions.map(item => [item.version, item.minOSVersion]), [["1.2.0", "17.0"], ["1.0.0", ios.minOSVersion]]);
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
  assert.deepEqual(JSON.parse(state.contents()["alt-source.json"]).apps[0].versions.map(item => item.version), ["1.3.0", "1.0.0"]);
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
