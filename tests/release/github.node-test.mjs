import assert from "node:assert/strict";
import test from "node:test";
import { Readable } from "node:stream";
import { GitHubReleases } from "../../scripts/release/github.mjs";

function client(responses) {
  const calls = [];
  const waits = [];
  const github = new GitHubReleases("synthetic-token", async (url, options) => {
    calls.push({ url, ...options });
    assert.ok(responses.length, "unexpected GitHub request");
    return responses.shift();
  }, { wait: async ms => waits.push(ms) });
  return { github, calls, waits };
}
const json = (value, status = 200) => new Response(JSON.stringify(value), { status });

test("GitHub reads recover from transient HTTP failures without retrying absent releases or mutations", async () => {
  const { github, calls, waits } = client([json({}, 504), json({}, 503), json({ id: 7 })]);
  assert.equal((await github.getLatest()).id, 7);
  assert.equal(calls.length, 3);
  assert.deepEqual(waits, [10000, 20000]);
  assert.equal(new Set(calls.map(call => call.signal)).size, 3);
  const absent = client([json({}, 404)]);
  assert.equal(await absent.github.getLatest(), null);
  assert.equal(absent.calls.length, 1);
  for (const method of ["POST", "PATCH", "DELETE"]) {
    const mutation = client([json({}, 503)]);
    await assert.rejects(mutation.github.request("/releases/7", { method }), new RegExp(`github-${method.toLowerCase()}-503`));
    assert.equal(mutation.calls.length, 1);
    assert.deepEqual(mutation.waits, []);
  }
});

test("GitHub binary reads restart after stream failure and never multiply retry attempts", async () => {
  const release = { assets: [{ id: 3, name: "manifest.json", state: "uploaded", size: 2 }] };
  const partial = new Response(Readable.toWeb(Readable.from((async function* () {
    yield Buffer.from("{");
    throw Object.assign(new Error("reset"), { code: "ECONNRESET" });
  })())));
  const recovered = client([partial, new Response("{}")]);
  assert.equal((await recovered.github.downloadAsset(release, "manifest.json", 3)).toString(), "{}");
  assert.equal(recovered.calls.length, 2);
  const failed = client([json({}, 503), json({}, 503), json({}, 503)]);
  await assert.rejects(failed.github.downloadAsset(release, "manifest.json", 3), /github-get-503/);
  assert.equal(failed.calls.length, 3);
  assert.deepEqual(failed.waits, [10000, 20000]);
});

test("GitHub JSON reads retry broken streams but not invalid response data", async () => {
  const partial = new Response(Readable.toWeb(Readable.from((async function* () {
    yield Buffer.from("{");
    throw Object.assign(new Error("reset"), { code: "ECONNRESET" });
  })())));
  const recovered = client([partial, json({ id: 7 })]);
  assert.equal((await recovered.github.getLatest()).id, 7);
  assert.equal(recovered.calls.length, 2);
  const invalid = client([new Response("not JSON")]);
  await assert.rejects(invalid.github.getLatest(), SyntaxError);
  assert.equal(invalid.calls.length, 1);
});

for (const [tag, name] of [["app-v1.0.0", "RisuNest v1.0.0"], ["sync-v1.0.0", "RisuNest Sync v1.0.0"]]) {
  test(`draft creation names ${tag}, resolves annotated tags and records the tested commit`, async () => {
    const commit = "a".repeat(40);
    const { github, calls } = client([json({}, 404), json([]), json({ object: { type: "tag", sha: "b".repeat(40) } }),
      json({ object: { type: "commit", sha: commit } }), json({ id: 7, draft: true })]);
    const notes = `# ${name}\n\nRelease notes`;
    assert.equal((await github.prepareDraft(tag, commit, notes)).id, 7);
    assert.deepEqual(JSON.parse(calls.at(-1).body), { tag_name: tag, target_commitish: commit,
      name, body: notes, draft: true, prerelease: false, make_latest: "false" });
  });
}

test("mismatched tag commits and API errors cannot create a release", async () => {
  const { github, calls } = client([json({}, 404), json([]), json({ object: { type: "commit", sha: "b".repeat(40) } })]);
  await assert.rejects(github.prepareDraft("app-v1.0.0", "a".repeat(40)), /tag-commit-mismatch/);
  assert.ok(calls.every(call => call.method === "GET"));
  const unavailable = client([json({ message: "synthetic secret" }, 403)]);
  await assert.rejects(unavailable.github.getLatest(), error => error.message === "github-get-403");
});

test("an existing draft still requires the tag to resolve to the tested commit", async () => {
  const commit = "a".repeat(40);
  const { github, calls } = client([json({}, 404), json([{ id: 7, tag_name: "app-v1.0.0", draft: true, target_commitish: commit, prerelease: false }]),
    json({ object: { type: "commit", sha: "b".repeat(40) } })]);
  await assert.rejects(github.prepareDraft("app-v1.0.0", commit), /tag-commit-mismatch/);
  assert.ok(calls.every(call => call.method === "GET"));
});

test("asset replacement rechecks the release and refuses published assets", async () => {
  const { github, calls } = client([json({ draft: false })]);
  await assert.rejects(github.uploadAsset({ id: 7, draft: true }, "manifest.json", Buffer.from("{}")), /immutable-release/);
  assert.equal(calls.length, 1);
});

test("asset reads enforce declared and streamed size limits", async () => {
  const release = { assets: [{ id: 3, name: "manifest.json", state: "uploaded", size: 2 }] };
  const { github } = client([new Response("{}"), new Response("{}extra"), new Response("{")]);
  assert.equal((await github.downloadAsset(release, "manifest.json", 3)).toString(), "{}");
  await assert.rejects(github.downloadAsset(release, "manifest.json", 3), /too-large/);
  await assert.rejects(github.downloadAsset(release, "manifest.json", 3), /size-mismatch/);
  await assert.rejects(github.downloadAsset(release, "missing", 3), /missing/);
});

 test("a rerun reuses the single matching draft found after pagination", async () => {
  const commit = "a".repeat(40);
  const draft = { id: 7, tag_name: "app-v1.0.0", draft: true, target_commitish: commit, prerelease: false };
  const { github, calls } = client([json({}, 404), json(Array.from({ length: 100 }, (_, id) => ({ id, draft: false }))),
    json([draft]), json({ object: { type: "commit", sha: commit } })]);
  assert.equal((await github.prepareDraft(draft.tag_name, commit)).id, 7);
  assert.match(calls[2].url, /page=2$/);
  assert.ok(calls.every(call => call.method === "GET"));
});

test("duplicate drafts and mismatched draft identity are rejected without mutation", async () => {
  const commit = "a".repeat(40);
  const draft = { id: 7, tag_name: "app-v1.0.0", draft: true, target_commitish: commit };
  for (const [drafts, error] of [[[draft, { ...draft, id: 8 }], /duplicate-release-drafts/],
    [[{ ...draft, target_commitish: "b".repeat(40) }], /release-identity-conflict/]]) {
    const { github, calls } = client([json({}, 404), json(drafts)]);
    await assert.rejects(github.prepareDraft(draft.tag_name, commit), error);
    assert.ok(calls.every(call => call.method === "GET"));
  }
});
