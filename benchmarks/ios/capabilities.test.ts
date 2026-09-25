import { readFileSync } from "node:fs";
import { resolve } from "node:path";
import { describe, expect, it } from "vitest";

function iosNativePermissions(path: string) {
  const capability = JSON.parse(readFileSync(path, "utf8")) as {
    permissions: (string | object)[];
  };
  return capability.permissions.filter(
    (permission): permission is string =>
      typeof permission === "string" && permission.startsWith("ios-native:"),
  );
}

describe("iOS benchmark capability", () => {
  it("allows every iOS native command available to the product frontend", () => {
    const product = iosNativePermissions(
      resolve("src-tauri/capabilities/ios.json"),
    );
    const benchmark = new Set(
      iosNativePermissions(
        resolve("benchmarks/ios/native/capabilities/ios.json"),
      ),
    );

    expect(product.filter((permission) => !benchmark.has(permission))).toEqual([]);
  });
});
