import { resolve } from "node:path";
import { pathToFileURL } from "node:url";
import { CATALOG_LIMIT, REPOSITORY, validateCatalog } from "./contracts.mjs";
import { GitHubReleases } from "./github.mjs";
import { renderDownloadTable } from "./download-table.mjs";
import { verifiedJson } from "./publication.mjs";

const files = [["README.md", "en"], ["README.ko.md", "ko"]];
function downloadSection(release, locale) {
  const { product, version, releasePage } = release;
  const link = `[RisuNest${product === "sync" ? " Sync" : ""} v${version}](${releasePage})`;
  const introduction = locale === "ko"
    ? product === "app"
      ? `${link}에서 운영체제와 CPU 아키텍처에 맞는 파일을 다운로드하세요.`
      : `${link}을 다운로드할 수 있습니다. Sync는 앱과 별도로 릴리스됩니다.`
    : product === "app"
      ? `Download ${link} for your operating system and CPU architecture.`
      : `Download ${link}. Sync is released independently from the app.`;
  return [introduction, "", renderDownloadTable(release, locale)].join("\n");
}

export function renderReadme(contents, catalog, locale) {
  validateCatalog(catalog);
  if (!["en", "ko"].includes(locale)) throw new Error("unsupported-readme-locale");
  const newline = contents.includes("\r\n") ? "\r\n" : "\n";
  const blocks = ["app", "sync"].map(product => {
    const start = `<!-- release-downloads:${product}:start -->`;
    const end = `<!-- release-downloads:${product}:end -->`;
    const from = contents.indexOf(start), to = contents.indexOf(end);
    if (from < 0 || to < from + start.length || contents.indexOf(start, from + 1) !== -1
      || contents.indexOf(end, to + 1) !== -1) throw new Error(`invalid-readme-markers:${product}`);
    const release = catalog.products[product]?.release;
    if (!release) throw new Error(`readme-product-unpublished:${product}`);
    return { from, to: to + end.length,
      text: `${start}\n${downloadSection(release, locale)}\n${end}`.replace(/\n/g, newline) };
  }).sort((left, right) => left.from - right.from);
  if (blocks[0].to > blocks[1].from) throw new Error("overlapping-readme-markers");
  for (const block of blocks.reverse()) contents = contents.slice(0, block.from) + block.text + contents.slice(block.to);
  return contents;
}

export async function loadReadmeCatalog(github, publicKey) {
  const latest = await github.getLatest();
  if (!latest || latest.draft !== false || latest.prerelease !== false) throw new Error("readme-stable-release-missing");
  const [bytes, signature] = await Promise.all([
    github.downloadAsset(latest, "manifest.json", CATALOG_LIMIT),
    github.downloadAsset(latest, "manifest.json.sig", 16384),
  ]);
  const catalog = validateCatalog(verifiedJson(bytes, signature.toString("utf8"), publicKey, CATALOG_LIMIT));
  if (catalog.publicationTag !== latest.tag_name) throw new Error("release-catalog-tag-mismatch");
  for (const product of ["app", "sync"]) {
    const entry = catalog.products[product];
    if (!entry) throw new Error(`readme-product-unpublished:${product}`);
    const release = entry.release;
    const published = release.tag === latest.tag_name ? latest : await github.getByTag(release.tag);
    if (!published || published.draft !== false || published.prerelease !== false || published.tag_name !== release.tag) {
      throw new Error(`readme-product-unpublished:${product}`);
    }
    for (const download of release.downloads) {
      const asset = published.assets?.find(item => item.browser_download_url === download.url);
      if (!asset || asset.state !== "uploaded" || asset.size !== download.size) {
        throw new Error(`readme-published-asset-mismatch:${product}`);
      }
    }
  }
  return catalog;
}

export async function updateReadmeDownloads({ github, publicKey }) {
  for (let attempt = 0; attempt < 3; attempt++) {
    const catalog = await loadReadmeCatalog(github, publicKey);
    const head = await github.request("/git/ref/heads/main");
    const parent = head.object.sha;
    const commit = await github.request(`/git/commits/${parent}`);
    const tree = await github.request(`/git/trees/${commit.tree.sha}`);
    if (tree.truncated) throw new Error("readme-tree-truncated");
    const changes = [];
    for (const [path, locale] of files) {
      const entry = tree.tree.find(item => item.path === path);
      if (!entry || entry.type !== "blob" || entry.mode !== "100644") throw new Error(`readme-file-missing:${path}`);
      const blob = await github.request(`/git/blobs/${entry.sha}`);
      if (blob.encoding !== "base64") throw new Error("readme-blob-encoding");
      const contents = new TextDecoder("utf-8", { fatal: true }).decode(Buffer.from(blob.content, "base64"));
      const content = renderReadme(contents, catalog, locale);
      if (content !== contents) changes.push({ path, mode: entry.mode, type: "blob", content });
    }
    if (!changes.length) return { status: "unchanged", commit: parent };
    const updatedTree = await github.request("/git/trees", {
      method: "POST", body: { base_tree: commit.tree.sha, tree: changes },
    });
    const updatedCommit = await github.request("/git/commits", {
      method: "POST", body: { message: "docs: update README download links for published releases",
        tree: updatedTree.sha, parents: [parent] },
    });
    try {
      await github.request("/git/refs/heads/main", {
        method: "PATCH", body: { sha: updatedCommit.sha, force: false },
      });
      return { status: "updated", commit: updatedCommit.sha };
    } catch (error) {
      const observed = await github.request("/git/ref/heads/main");
      if (observed.object.sha === updatedCommit.sha) return { status: "updated", commit: updatedCommit.sha };
      if (observed.object.sha === parent) throw error;
    }
  }
  throw new Error("readme-branch-kept-changing");
}

async function main() {
  if (process.env.GITHUB_REPOSITORY !== REPOSITORY) throw new Error("readme-repository-mismatch");
  const publicKey = process.env.RISUNEST_UPDATE_PUBLIC_KEY;
  if (!publicKey) throw new Error("readme-public-key-required");
  const github = new GitHubReleases(process.env.GITHUB_TOKEN);
  console.log(JSON.stringify(await updateReadmeDownloads({ github, publicKey })));
}

if (process.argv[1] && import.meta.url === pathToFileURL(resolve(process.argv[1])).href) {
  main().catch(error => { console.error(error.message); process.exitCode = 1; });
}
