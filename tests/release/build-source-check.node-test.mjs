import assert from "node:assert/strict";
import { mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { createRequire } from "node:module";
import { tmpdir } from "node:os";
import { join } from "node:path";
import test from "node:test";
import { runInNewContext } from "node:vm";
import { androidVersionCode, iosBundleVersion } from "../../scripts/release/common.mjs";
import { releaseDownloads } from "../../scripts/release/downloads.mjs";
import { sourceCheck, validateVendorInput } from "../../scripts/release/source-check.mjs";
import cloudflared from "../../scripts/release/cloudflared.json" with { type: "json" };
import { releaseTauriConfig } from "../../scripts/release/tauri-config.mjs";

const require = createRequire(import.meta.url);
const workflow = readFileSync(new URL("../../.github/workflows/release.yml", import.meta.url), "utf8").replace(/\r\n/g, "\n");

function sourceIdentity({ repository, product, tag = "", publish = false, eventName = "workflow_dispatch", refType, refName }, runGit) {
  const step = /      - id: identity\n([\s\S]*?)(?=      - )/.exec(workflow)?.[1];
  assert(step, "Missing release source identity step");
  const inline = /        run: \|\n([\s\S]*?)(?=        env:|$)/.exec(step)?.[1]
    .split("\n").map(line => line.replace(/^          /, "")).join("\n").trim();
  const program = inline
    ? /^node --input-type=commonjs <<'NODE'\n([\s\S]*)\nNODE$/.exec(inline)?.[1]
    : readFileSync(join(repository, /        run: node (\S+)/.exec(step)?.[1]), "utf8");
  assert(program, "Missing executable source identity program");
  const output = join(repository, "identity-output");
  writeFileSync(output, "");
  runInNewContext(program, {
    require: name => name === "node:child_process" ? { execFileSync: (_file, args) => runGit(args) } : require(name),
    process: { cwd: () => repository, env: { PRODUCT: product, TAG: tag, PUBLISH: String(publish),
      GITHUB_EVENT_NAME: eventName, GITHUB_REF_TYPE: refType, GITHUB_REF_NAME: refName, GITHUB_OUTPUT: output } },
    console: { log() {} },
  }, { timeout: 1000 });
  const values = Object.fromEntries(readFileSync(output, "utf8").trim().split("\n").map(line => line.split("=")));
  return { tag: values.tag, sourceCommit: values.source_commit };
}

function write(path, value = "locked\n") {
  mkdirSync(join(path, ".."), { recursive: true });
  writeFileSync(path, value);
}

function json(path, value) {
  write(path, `${JSON.stringify(value)}\n`);
}

function repository(product, version) {
  const root = mkdtempSync(join(tmpdir(), "risunest-source-check-"));
  write(join(root, "crates/release-update/Cargo.lock"));
  write(join(root, "release-notes", product, `${version}.md`), [
    `# ${product === "app" ? "RisuNest" : "RisuNest Sync"} v${version}`,
    "",
    "## English",
    "",
    "- Initial release.",
    "",
    "## 한국어",
    "",
    "- 최초 릴리즈.",
    "",
  ].join("\n"));
  if (product === "app") {
    json(join(root, "version.json"), { version });
    json(join(root, "src-tauri/tauri.conf.json"), { version });
    write(join(root, "pnpm-lock.yaml"));
    write(join(root, "src-tauri/Cargo.lock"));
  } else {
    json(join(root, "server/version.json"), {
      version,
      compatibility: {
        protocolId: "risunest-sync/v1",
        storeFormatId: "risunest-sync-store/v8",
        automaticApply: true,
      },
    });
    for (const file of [
      "crates/sync-wire/Cargo.lock",
      "server/sync/Cargo.lock",
      "server/manager/Cargo.lock",
      "server/manager/gui/pnpm-lock.yaml",
      "server/manager/gui/src-tauri/Cargo.lock",
    ]) write(join(root, file));
    write(join(root, "crates/sync-wire/src/lib.rs"), 'pub const PROTOCOL_ID: &str = "risunest-sync/v1";');
    write(join(root, "server/sync/src/lib.rs"), [
      "pub use risunest_sync_wire::PROTOCOL_ID;",
      'pub const STORE_FORMAT_ID: &str = "risunest-sync-store/v8";',
    ].join("\n"));
    for (const file of [
      "server/sync/Cargo.toml",
      "server/manager/Cargo.toml",
      "server/manager/gui/src-tauri/Cargo.toml",
    ]) write(join(root, file), `[package]\nname = "synthetic"\nversion = "${version}"\n`);
    json(join(root, "server/manager/gui/package.json"), { version });
    json(join(root, "server/manager/gui/src-tauri/tauri.conf.json"), { version });
  }
  return root;
}

function gitState({ head = "a".repeat(40), tags = {}, contained = true } = {}) {
  return args => {
    if (args.join(" ") === "rev-parse HEAD") return head;
    if (args.join(" ") === `merge-base --is-ancestor ${head} refs/remotes/origin/main`) {
      if (!contained) throw new Error("source-outside-main");
      return "";
    }
    const tag = /^rev-parse --verify refs\/tags\/(.+)\^\{commit\}$/.exec(args.join(" "))?.[1];
    if (tag) {
      if (!tags[tag]) throw new Error("tag-not-found");
      return tags[tag];
    }
    throw new Error(`Unexpected Git operation: ${args.join(" ")}`);
  };
}

test("workflow rehearsals reuse version and notes from a source without the workflow's helper files", t => {
  for (const product of ["app", "sync"]) {
    const root = repository(product, "1.2.3");
    t.after(() => rmSync(root, { recursive: true, force: true }));
    const expectedTag = `${product}-v1.2.3`;
    for (const tag of [undefined, "", expectedTag]) {
      for (const sourceCommit of ["b".repeat(40), "c".repeat(40)]) {
        const identity = sourceIdentity({ repository: root, product, tag, publish: false, eventName: "workflow_dispatch" },
          gitState({ head: sourceCommit, tags: { [expectedTag]: "a".repeat(40) } }));
        assert.deepEqual(identity, { tag: expectedTag, sourceCommit });
        const input = sourceCheck({ repository: root, product, ...identity,
          publishedAt: "2026-10-15T00:00:00Z", registryUrl: "https://sync.example.invalid/" });
        assert.equal(input.version, "1.2.3");
        assert.equal(input.tag, expectedTag);
        assert.equal(input.sourceCommit, sourceCommit);
        assert.equal(input.localizedNotes.en, "- Initial release.");
      }
    }
    assert.deepEqual(sourceIdentity({ repository: root, product }, gitState()),
      { tag: expectedTag, sourceCommit: "a".repeat(40) });
  }
});

test("publication still requires an explicit existing tag on the exact source commit", t => {
  const root = repository("app", "1.2.3");
  t.after(() => rmSync(root, { recursive: true, force: true }));
  const input = { repository: root, product: "app", publish: true, eventName: "workflow_dispatch" };
  assert.throws(() => sourceIdentity(input, gitState()), /publication-tag-required/);
  input.tag = "app-v1.2.3";
  assert.throws(() => sourceIdentity(input, gitState()), /tag-not-found/);
  assert.throws(() => sourceIdentity(input, gitState({ tags: { [input.tag]: "b".repeat(40) } })), /release-tag-commit-mismatch/);
  assert.deepEqual(sourceIdentity(input, gitState({ tags: { [input.tag]: "a".repeat(40) } })),
    { tag: input.tag, sourceCommit: "a".repeat(40) });
});

test("rehearsal and publication both reject sources outside main and mismatched product versions", t => {
  const root = repository("sync", "1.2.3");
  t.after(() => rmSync(root, { recursive: true, force: true }));
  for (const publish of [false, true]) {
    const input = { repository: root, product: "sync", tag: "sync-v1.2.3", publish, eventName: "workflow_dispatch" };
    assert.throws(() => sourceIdentity(input, gitState({ contained: false })), /source-outside-main/);
    assert.throws(() => sourceIdentity({ ...input, tag: "sync-v1.2.4" }, gitState()), /Expected tag/);
    assert.throws(() => sourceIdentity({ ...input, tag: "app-v1.2.3" }, gitState()), /does not belong to sync/);
  }
});

test("tag pushes must identify the exact checked-out release tag", t => {
  const root = repository("app", "1.2.3");
  t.after(() => rmSync(root, { recursive: true, force: true }));
  const input = { repository: root, product: "app", tag: "app-v1.2.3", publish: true,
    eventName: "push", refType: "tag", refName: "app-v1.2.3" };
  const git = gitState({ tags: { [input.tag]: "a".repeat(40) } });
  assert.deepEqual(sourceIdentity(input, git), { tag: input.tag, sourceCommit: "a".repeat(40) });
  assert.throws(() => sourceIdentity({ ...input, refType: "branch" }, git), /pushed-release-tag-mismatch/);
  assert.throws(() => sourceIdentity({ ...input, refName: "app-v1.2.4" }, git), /pushed-release-tag-mismatch/);
  assert.throws(() => sourceIdentity({ ...input, publish: false }, gitState({ tags: { [input.tag]: "b".repeat(40) } })),
    /release-tag-commit-mismatch/);
});

test("source check binds an app tag, commit, notes and mobile build number", () => {
  const root = repository("app", "2026.8.250");
  const result = sourceCheck({
    repository: root,
    product: "app",
    tag: "app-v2026.8.250",
    sourceCommit: "a".repeat(40),
    publishedAt: "2026-09-15T00:00:00Z",
  });
  assert.equal(result.expectedDownloads.length, 14);
  assert.equal(result.mobileBuildNumber, 2_026_008_250);
  assert.equal(result.iosBuildNumber, "2026.8.250");
  assert.equal(result.compatibility, null);
  assert.match(result.notes, /^# RisuNest v2026\.8\.250\n[\s\S]*## English[\s\S]*## 한국어/);
  assert.deepEqual(result.localizedNotes, {
    en: "- Initial release.",
    ko: "- 최초 릴리즈.",
  });
});

test("source check carries authoritative Sync compatibility and pinned vendor input", () => {
  const root = repository("sync", "0.1.0");
  const result = sourceCheck({
    repository: root,
    product: "sync",
    tag: "sync-v0.1.0",
    sourceCommit: "b".repeat(40),
    publishedAt: "2026-09-15T00:00:00Z",
    registryUrl: "https://sync.example.invalid/registry/",
  });
  assert.equal(result.expectedDownloads.length, 16);
  assert.match(result.notes, /^# RisuNest Sync v0\.1\.0\n/);
  assert.deepEqual(result.localizedNotes, {
    en: "- Initial release.",
    ko: "- 최초 릴리즈.",
  });
  assert.equal(result.compatibility.storeFormatId, "risunest-sync-store/v8");
  assert.equal(result.vendorInput.version, "2026.9.1");
  assert.equal(result.vendorInput.assets["windows-aarch64"].actualArch, "x86_64");
});

test("source check rejects tag drift, missing notes and incomplete compatibility", () => {
  const app = repository("app", "1.2.3");
  assert.throws(() => sourceCheck({
    repository: app,
    product: "app",
    tag: "app-v1.2.4",
    sourceCommit: "c".repeat(40),
    publishedAt: "2026-09-15T00:00:00Z",
  }), /Expected tag/);
  const sync = repository("sync", "1.2.3");
  json(join(sync, "server/version.json"), { version: "1.2.3" });
  assert.throws(() => sourceCheck({
    repository: sync,
    product: "sync",
    tag: "sync-v1.2.3",
    sourceCommit: "d".repeat(40),
    publishedAt: "2026-09-15T00:00:00Z",
    registryUrl: "https://sync.example.invalid/",
  }), /compatibility/);
});

test("source check rejects missing, duplicate, or empty localized release-note sections", () => {
  const cases = [
    ["missing Korean", "# app 1.2.3\n\n## English\n\n- Ready.\n"],
    ["duplicate English", "# app 1.2.3\n\n## English\n\n- One.\n\n## English\n\n- Two.\n\n## 한국어\n\n- 준비됨.\n"],
    ["empty English", "# app 1.2.3\n\n## English\n\n## 한국어\n\n- 준비됨.\n"],
  ];
  for (const [label, notes] of cases) {
    const app = repository("app", "1.2.3");
    write(join(app, "release-notes/app/1.2.3.md"), notes);
    assert.throws(() => sourceCheck({
      repository: app,
      product: "app",
      tag: "app-v1.2.3",
      sourceCommit: "f".repeat(40),
      publishedAt: "2026-09-15T00:00:00Z",
    }), /localized release notes/, label);
  }
});

test("source check requires the directly tested sync-wire lockfile", () => {
  const sync = repository("sync", "1.2.3");
  rmSync(join(sync, "crates/sync-wire/Cargo.lock"));
  assert.throws(() => sourceCheck({
    repository: sync,
    product: "sync",
    tag: "sync-v1.2.3",
    sourceCommit: "e".repeat(40),
    publishedAt: "2026-09-15T00:00:00Z",
    registryUrl: "https://sync.example.invalid/",
  }), /Lockfile is missing/);
});

test("source check compares Sync compatibility with the sync-wire protocol constant", () => {
  const sync = repository("sync", "1.2.3");
  write(join(sync, "crates/sync-wire/src/lib.rs"), 'pub const PROTOCOL_ID: &str = "risunest-sync/v2";');
  assert.throws(() => sourceCheck({
    repository: sync,
    product: "sync",
    tag: "sync-v1.2.3",
    sourceCommit: "e".repeat(40),
    publishedAt: "2026-09-15T00:00:00Z",
    registryUrl: "https://sync.example.invalid/",
  }), /does not match daemon constants/);
});

test("both products require the shared native update test lockfile", () => {
  for (const product of ["app", "sync"]) {
    const root = repository(product, "1.2.3");
    rmSync(join(root, "crates/release-update/Cargo.lock"));
    assert.throws(() => sourceCheck({
      repository: root,
      product,
      tag: `${product}-v1.2.3`,
      sourceCommit: "e".repeat(40),
      publishedAt: "2026-09-15T00:00:00Z",
      registryUrl: "https://sync.example.invalid/",
    }), /Lockfile is missing/);
  }
});

test("release names are unique and Android build numbers are bounded", () => {
  for (const product of ["app", "sync"]) {
    const names = releaseDownloads(product, "1.2.3").map((download) => download.fileName);
    assert.equal(new Set(names).size, names.length);
  }
  assert.throws(() => androidVersionCode("2100.0.0"), /versionCode/);
  assert.equal(iosBundleVersion("2026.8.250"), "2026.8.250");
});

test("pinned cloudflared inputs reject a wrong native architecture", () => {
  const input = structuredClone(cloudflared);
  input.assets["linux-aarch64"].actualArch = "x86_64";
  assert.throws(() => validateVendorInput(input), /architecture.*linux-aarch64/);
});

test("release Tauri config leaves updater discovery to the verified native boundary", () => {
  const config = releaseTauriConfig({ product: "app", version: "1.2.3", mobileBuildNumber: 1_002_003, iosBuildNumber: "1.2.3" }, "synthetic-public-key");
  assert.deepEqual(config.build.features, ["official-distribution"]);
  assert.deepEqual(config.plugins.updater.endpoints, []);
  assert.equal(JSON.stringify(config).includes("releases/latest/download/manifest.json"), false);
  assert.equal(releaseTauriConfig({ product: "sync", version: "1.2.3" }, "synthetic-public-key").build, undefined);
});
