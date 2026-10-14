import { readFileSync } from "node:fs";
import { resolve } from "node:path";
import { describe, expect, it } from "vitest";
const source = (file: string) => readFileSync(resolve(file), "utf8");
describe("server-only product boundaries", () => {
  it("preserves generation keep-alive, SAF and OS deep-link registration", () => {
    const activity = source(
      "src-tauri/gen/android/app/src/main/java/io/github/rsyumi/risunest/MainActivity.kt",
    );
    expect(activity).toContain("GenerationKeepAliveBridge");
    expect(activity).toContain("Saf");
    const manifest = source(
      "src-tauri/gen/android/app/src/main/AndroidManifest.xml",
    );
    expect(manifest).toContain(".GenerationForegroundService");
    expect(manifest).toContain('android:scheme="risunestlocal"');
    expect(manifest).toContain(
      "android.permission.FOREGROUND_SERVICE_DATA_SYNC",
    );
  });
});
