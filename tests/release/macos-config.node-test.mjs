import assert from "node:assert/strict";
import { mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import test from "node:test";
import { packageApp } from "../../scripts/release/package-app.mjs";

test("Darwin packaging resolves the macOS overlay before checking the bundle", () => {
  const output = mkdtempSync(join(tmpdir(), "risunest-package-config-"));
  try {
    assert.throws(() => packageApp({ kind: "desktop", os: "darwin", arch: "aarch64", output,
      binary: join(output, "app"), bundleDirectory: output, releaseInput: { version: "1.0.0" } }),
      /Expected one macOS updater archive/);
  } finally { rmSync(output, { recursive: true, force: true }); }
});
