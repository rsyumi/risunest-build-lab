import { resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { parseArgs, readJson, requireArg, writeJson } from "./common.mjs";

export function mergeTauriConfig(base, ...overrides) {
  const result = structuredClone(base);
  for (const override of overrides) {
    for (const [key, value] of Object.entries(override ?? {})) {
      if (value === null) delete result[key];
      else if (typeof value === "object" && !Array.isArray(value))
        result[key] = mergeTauriConfig(result[key] ?? {}, value);
      else result[key] = structuredClone(value);
    }
  }
  return result;
}

export const APP_IDENTIFIER = "io.github.rsyumi.risunest";
export const SYNC_IDENTIFIER = "io.github.rsyumi.risunest.sync-manager";

export function assertAppIdentifier(config, os) {
  if (!["windows", "linux", "macos", "android", "ios"].includes(os))
    throw new Error(`Unexpected effective ${os} app identifier: ${config.identifier}`);
  if (config.identifier !== APP_IDENTIFIER)
    throw new Error(`Unexpected effective ${os} app identifier: ${config.identifier}`);
  return APP_IDENTIFIER;
}

export function assertSyncIdentifier(config, os) {
  if (!["windows", "linux", "macos"].includes(os))
    throw new Error(`Unexpected effective ${os} Sync identifier: ${config.identifier}`);
  if (config.identifier !== SYNC_IDENTIFIER)
    throw new Error(`Unexpected effective ${os} Sync identifier: ${config.identifier}`);
  return SYNC_IDENTIFIER;
}

// The identifier is an identity key, not a naming knob. A platform override
// would move the bundle id, the WebView profile, and four Tauri directories at
// once, so no overlay may set one.
export function assertNoIdentifierOverride(overlay, label) {
  if (overlay && Object.hasOwn(overlay, "identifier"))
    throw new Error(`${label} must not override identifier: ${overlay.identifier}`);
}

export function releaseTauriConfig(releaseInput, publicKey) {
  if (!publicKey) throw new Error("RISUNEST_UPDATE_PUBLIC_KEY is required.");
  const config = {
    version: releaseInput.version,
    bundle: { createUpdaterArtifacts: true },
  };
  if (releaseInput.product === "app" && releaseInput.mobileBuildNumber) {
    config.bundle.android = { versionCode: releaseInput.mobileBuildNumber };
    config.bundle.iOS = { bundleVersion: releaseInput.iosBuildNumber };
  }
  if (releaseInput.product === "app") {
    config.plugins = {
      updater: {
        pubkey: publicKey,
        endpoints: [],
      },
    };
  }
  return config;
}

function main() {
  const args = parseArgs(process.argv.slice(2));
  const input = readJson(requireArg(args, "release-input"));
  writeJson(
    requireArg(args, "output"),
    releaseTauriConfig(input, process.env.RISUNEST_UPDATE_PUBLIC_KEY),
  );
}

if (process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url)) main();
