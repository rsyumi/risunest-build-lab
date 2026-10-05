import assert from "node:assert/strict";
import { mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { createRequire } from "node:module";
import path from "node:path";
import test from "node:test";

const require = createRequire(import.meta.url);
const tailwindRequire = createRequire(require.resolve("@tailwindcss/vite"));
const { compile } = tailwindRequire("@tailwindcss/node");
const { Scanner } = tailwindRequire("@tailwindcss/oxide");

test("application CSS keeps source and startup utilities without scanning unrelated root files", async () => {
  const root = process.cwd();
  const probe = mkdtempSync(path.join(root, "tailwind-source-probe-"));
  try {
    writeFileSync(path.join(probe, "unrelated.html"), '<div class="m-[123456px]"></div>');
    const base = path.join(root, "src");
    const compiler = await compile(readFileSync(path.join(base, "styles.css"), "utf8"), {
      base,
      onDependency() {},
    });
    const sources = compiler.root === "none" ? [] : [
      compiler.root === null
        ? { base: root, pattern: "**/*", negated: false }
        : { ...compiler.root, negated: false },
    ];
    const scanner = new Scanner({ sources: [...sources, ...compiler.sources] });
    const css = compiler.build(scanner.scan());
    assert.ok(css.includes(".bg-darkbg"), "Application theme utility is missing");
    assert.ok(css.includes(".motion-reduce\\:animate-none"), "Startup reduced-motion utility is missing");
    assert.ok(css.includes("prefers-reduced-motion"), "Startup reduced-motion behavior is missing");
    assert.ok(!css.includes("123456px"), "An unrelated root file changed application CSS");
  } finally {
    assert.equal(path.dirname(probe), root);
    assert.ok(path.basename(probe).startsWith("tailwind-source-probe-"));
    rmSync(probe, { recursive: true, force: true });
  }
});
