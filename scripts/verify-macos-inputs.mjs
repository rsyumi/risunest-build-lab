import assert from "node:assert/strict";
import { readFileSync, readdirSync } from "node:fs";
import path from "node:path";
import { pathToFileURL } from "node:url";
const root = process.argv[2];
const read = (file) => JSON.parse(readFileSync(path.join(root, file)));
const base = read("src-tauri/tauri.conf.json");
const mac = read("src-tauri/tauri.macos.conf.json");
const agent = read("src-tauri/tauri.agent.conf.json");
assert.equal(base.identifier, "io.github.rsyumi.risunest");
assert.equal(base.productName, "RisuNest");
assert.equal(base.mainBinaryName, "RisuNest");
assert.equal(base.build.frontendDist, "../dist");
assert.equal(mac.bundle.macOS.minimumSystemVersion, "14.0");
assert.equal(agent.build.beforeBuildCommand, "pnpm tauribuild:agent");
assert.match(read("package.json").scripts["build:agent"], /--mode agent/);
const sources = [];
function visit(dir) {
  for (const entry of readdirSync(dir, { withFileTypes: true })) {
    const file = path.join(dir, entry.name);
    if (entry.isDirectory()) visit(file);
    else if (file.endsWith(".map"))
      sources.push(...JSON.parse(readFileSync(file)).sources);
  }
}
visit(path.join(root, "dist"));
assert(
  sources.some((s) => s.endsWith("/realmEndpoints.blocked.ts")),
  "agent endpoint replacement absent",
);
assert(
  !sources.some((s) => s.endsWith("/realmEndpoints.ts")),
  "live Realm endpoint module shipped",
);
const { isVerificationModule } = await import(
  pathToFileURL(path.join(path.resolve(root), "tests/productionBundle.mjs"))
);
assert(
  !sources.some(isVerificationModule),
  "verification module in production source maps",
);
console.log("Mac config and agent frontend boundary verified");
