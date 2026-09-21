import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import { existsSync, mkdirSync, mkdtempSync, readFileSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { fileURLToPath } from "node:url";
import { spawnSync } from "node:child_process";
import test from "node:test";
import { assertBinaryArchitecture } from "../../scripts/release/platform/native.mjs";
import { assertPackageFormat } from "../../scripts/release/platform/package.mjs";
import {
  assertIosBundleMetadata,
  assertMacBundleMetadata,
  assertMacAdHocSignature,
  inspectMacAppRoot,
  validateIosArchiveEntries,
} from "../../scripts/release/package-app.mjs";
import { APP_IDENTIFIER, SYNC_IDENTIFIER, assertAppIdentifier, assertNoIdentifierOverride, assertSyncIdentifier, mergeTauriConfig, releaseTauriConfig } from "../../scripts/release/tauri-config.mjs";
import { validateOwnedInventory } from "../../server/manager/install/package.mjs";
import { identifierDirectories, overlaps, windowsLayout } from "../pathManifest.mjs";

function pe(machine) {
  const bytes = Buffer.alloc(128);
  bytes.write("MZ");
  bytes.writeUInt32LE(64, 0x3c);
  bytes.write("PE\0\0", 64);
  bytes.writeUInt16LE(machine, 68);
  return bytes;
}

function elf(machine) {
  const bytes = Buffer.alloc(64);
  Buffer.from([0x7f, 0x45, 0x4c, 0x46, 2, 1]).copy(bytes);
  bytes.writeUInt16LE(machine, 18);
  return bytes;
}

function macho(cpu) {
  const bytes = Buffer.alloc(64);
  bytes.writeUInt32LE(0xfeedfacf, 0);
  bytes.writeUInt32LE(cpu, 4);
  return bytes;
}

function universalMacho(cpus) {
  const bytes = Buffer.alloc(8 + cpus.length * 20);
  bytes.writeUInt32BE(0xcafebabe, 0);
  bytes.writeUInt32BE(cpus.length, 4);
  cpus.forEach((cpu, index) => bytes.writeUInt32BE(cpu, 8 + index * 20));
  return bytes;
}

test("native executable checks distinguish both release architectures", () => {
  const root = mkdtempSync(join(tmpdir(), "risunest-native-"));
  const fixtures = [
    ["x64.exe", pe(0x8664), "x86_64", "pe"],
    ["arm.exe", pe(0xaa64), "aarch64", "pe"],
    ["x64.elf", elf(62), "x86_64", "elf"],
    ["arm.elf", elf(183), "aarch64", "elf"],
    ["x64.macho", macho(0x01000007), "x86_64", "macho"],
    ["arm.macho", macho(0x0100000c), "aarch64", "macho"],
  ];
  for (const [name, bytes, arch, format] of fixtures) {
    const path = join(root, name);
    writeFileSync(path, bytes);
    assert.deepEqual(assertBinaryArchitecture(path, arch, format), { format, arches: [arch] });
  }
  assert.throws(() => assertBinaryArchitecture(join(root, "arm.exe"), "x86_64", "pe"), /expected x86_64/);
  const universal = join(root, "universal.macho");
  writeFileSync(universal, universalMacho([0x01000007, 0x0100000c]));
  assert.throws(() => assertBinaryArchitecture(universal, "aarch64", "macho"), /targets x86_64,aarch64/);
});

test("release package checks reject extension-only fakes", () => {
  const root = mkdtempSync(join(tmpdir(), "risunest-package-"));
  const deb = join(root, "app.deb");
  writeFileSync(deb, Buffer.from("!<arch>\ncontent"));
  assert.equal(assertPackageFormat(deb, "deb").format, "deb");
  const fake = join(root, "fake.deb");
  writeFileSync(fake, "not a package");
  assert.throws(() => assertPackageFormat(fake, "deb"), /Debian archive/);
});

test("raw packaging accepts native Windows ARM64 targets", () => {
  const root = mkdtempSync(join(tmpdir(), "risunest-raw-package-"));
  const binary = join(root, "risunest-sync-server.exe");
  const output = join(root, "out");
  writeFileSync(binary, pe(0xaa64));
  const result = spawnSync("python", [
    "server/sync/distribution/package.py",
    "--binary", binary,
    "--target", "aarch64-pc-windows-msvc",
    "--version", "1.2.3",
    "--output", output,
  ], { cwd: new URL("../..", import.meta.url), encoding: "utf8" });
  assert.equal(result.status, 0, result.stderr);
  const report = JSON.parse(result.stdout.trim());
  assert.equal(report.target, "aarch64-pc-windows-msvc");
  assert.ok(readFileSync(join(output, "risunest-sync-server-1.2.3-aarch64-pc-windows-msvc.zip")).length > 0);
});

test("NSIS inspection excludes only generated installer metadata from the owned inventory", () => {
  const root = mkdtempSync(join(tmpdir(), "risunest-nsis-inventory-"));
  const cloudflared = Buffer.from("synthetic cloudflared");
  const cloudflaredSha256 = createHash("sha256").update(cloudflared).digest("hex");
  writeFileSync(join(root, "cloudflared.exe"), cloudflared);
  writeFileSync(join(root, "risunest-sync-gui.exe"), pe(0xaa64));
  mkdirSync(join(root, "$PLUGINSDIR"));
  writeFileSync(join(root, "$PLUGINSDIR", "nsDialogs.dll"), "installer scaffold");
  writeFileSync(join(root, "uninstall.exe"), "generated uninstaller");
  const marker = join(root, "risunest-sync-bundle.json");
  writeFileSync(marker, JSON.stringify({
    schema: "risunest-sync-bundle/v1",
    product: "sync",
    variant: "managed",
    version: "1.2.3",
    protocolId: "risunest-sync/v1",
    storeFormatId: "risunest-sync-store/v8",
    files: ["cloudflared.exe", "risunest-sync-gui.exe"],
    vendor: [{
      name: "cloudflared",
      version: "2026.9.1",
      os: "windows",
      arch: "x86_64",
      sha256: cloudflaredSha256,
      path: "cloudflared.exe",
    }],
  }));
  const releaseInput = {
    version: "1.2.3",
    compatibility: {
      protocolId: "risunest-sync/v1",
      storeFormatId: "risunest-sync-store/v8",
    },
    vendorInput: { version: "2026.9.1" },
  };
  const build = { cloudflaredArch: "x86_64", cloudflaredSha256 };
  assert.throws(
    () => validateOwnedInventory(root, marker, releaseInput, build),
    /does not exactly list/,
  );
  assert.equal(
    validateOwnedInventory(root, marker, releaseInput, build, cloudflaredSha256, ["uninstall.exe"]).version,
    "1.2.3",
  );
  writeFileSync(join(root, "unexpected.exe"), "unexpected owned file");
  assert.throws(
    () => validateOwnedInventory(root, marker, releaseInput, build, cloudflaredSha256, ["uninstall.exe"]),
    /does not exactly list/,
  );
});

test("mounted DMG inspection rejects a mismatched app architecture", () => {
  const root = mkdtempSync(join(tmpdir(), "risunest-dmg-proof-"));
  const executable = join(root, "RisuNest.app", "Contents", "MacOS", "RisuNest");
  mkdirSync(join(executable, ".."), { recursive: true });
  writeFileSync(executable, Buffer.concat([macho(0x01000007), Buffer.from("__TAURI_BUNDLE_TYPE_VAR_APP")]));
  assert.throws(() => inspectMacAppRoot(root, "aarch64", join(root, "proof")), /expected aarch64/);
});

const tauriConfig = {
  identifier: "io.github.rsyumi.risunest",
  bundle: { fileAssociations: [{ ext: ["risum", "risudat"] }] },
  plugins: {
    "deep-link": {
      mobile: [
        { scheme: ["risunestlocal"], appLink: false },
        { scheme: ["https"], appLink: true },
      ],
    },
  },
};
const iosConfig = { bundle: { iOS: { minimumSystemVersion: "16.4" } } };
const releaseInput = { version: "2026.8.250", iosBuildNumber: "2026.8.250" };

function iosInfo() {
  return {
    CFBundleIdentifier: "io.github.rsyumi.risunest",
    CFBundleSupportedPlatforms: ["iPhoneOS"],
    MinimumOSVersion: "16.4",
    CFBundleShortVersionString: "2026.8.250",
    CFBundleVersion: "2026.8.250",
    CFBundleDocumentTypes: [{
      LSHandlerRank: "Default",
      CFBundleTypeRole: "Editor",
      CFBundleTypeName: "risum",
      CFBundleTypeExtensions: ["risum", "risudat"],
    }],
    CFBundleURLTypes: [{
      CFBundleURLName: "risunestlocal",
      CFBundleURLSchemes: ["risunestlocal"],
    }],
    CFBundleExecutable: "RisuNest",
  };
}

test("iOS package inventory accepts only one complete RisuNest app", () => {
  assert.deepEqual(validateIosArchiveEntries([
    "Payload/",
    "Payload/RisuNest.app/",
    "Payload/RisuNest.app/RisuNest",
    "Payload/RisuNest.app/Info.plist",
    "Payload/RisuNest.app/assets/index.html",
  ]), [
    "Payload/",
    "Payload/RisuNest.app/",
    "Payload/RisuNest.app/Info.plist",
    "Payload/RisuNest.app/RisuNest",
    "Payload/RisuNest.app/assets/index.html",
  ]);
  assert.throws(() => validateIosArchiveEntries([
    "Payload/RisuNest.app/Info.plist",
    "SwiftSupport/libswiftCore.dylib",
  ]), /only the Payload\/RisuNest.app bundle/);
  assert.throws(() => validateIosArchiveEntries([
    "Payload/RisuNest.app/Info.plist",
    "Payload/RisuNest.app/../escaped",
  ]), /unsafe path/);
  assert.throws(() => validateIosArchiveEntries([
    "Payload/RisuNest.app/Info.plist",
    "Payload/RisuNest.app/Info.plist",
  ]), /duplicate path/);
});

test("iOS bundle proof requires the configured custom URL scheme", () => {
  assert.equal(
    assertIosBundleMetadata(iosInfo(), releaseInput, tauriConfig, iosConfig).executable,
    "RisuNest",
  );
  const missingUrls = iosInfo();
  delete missingUrls.CFBundleURLTypes;
  assert.throws(
    () => assertIosBundleMetadata(missingUrls, releaseInput, tauriConfig, iosConfig),
    /bundle metadata does not match/,
  );
});

test("committed iOS shell carries the configured custom URL scheme", () => {
  const repository = fileURLToPath(new URL("../..", import.meta.url));
  const plistPath = join(repository, "src-tauri/gen/apple/risunest_iOS/Info.plist");
  const parsed = spawnSync("python", [
    "-c",
    "import json, plistlib, sys; print(json.dumps(plistlib.load(open(sys.argv[1], 'rb'))['CFBundleURLTypes']))",
    plistPath,
  ], { encoding: "utf8" });
  assert.equal(parsed.status, 0, parsed.stderr);
  const config = JSON.parse(readFileSync(join(repository, "src-tauri/tauri.conf.json"), "utf8"));
  const expected = config.plugins["deep-link"].mobile
    .filter((deepLink) => !deepLink.appLink)
    .map((deepLink) => ({
      CFBundleURLSchemes: deepLink.scheme.filter((scheme) => scheme !== "http" && scheme !== "https"),
      CFBundleURLName: deepLink.scheme[0],
    }));
  assert.deepEqual(JSON.parse(parsed.stdout), expected);
  const project = readFileSync(join(repository, "src-tauri/gen/apple/project.yml"), "utf8");
  assert.match(project, /CFBundleURLSchemes: \[risunestlocal\]\s+CFBundleURLName: risunestlocal/);
});


test("iOS package proof requires imported and exported custom document types", () => {
  const imported = "io.github.rsyumi.risunest.risum";
  const exported = "io.github.rsyumi.risunest.risunest";
  const associations = [
    { ext: ["risum"], contentTypes: [imported] },
    { ext: ["risunest"], mimeType: "application/x-risunest", exportedType: { identifier: exported, conformsTo: ["public.data"] } },
  ];
  const config = { ...tauriConfig, bundle: { fileAssociations: associations } };
  const info = { ...iosInfo(), CFBundleDocumentTypes: associations.map(a => ({
    CFBundleTypeExtensions: a.ext, CFBundleTypeName: a.ext[0], CFBundleTypeRole: "Editor", LSHandlerRank: "Default",
    LSItemContentTypes: a.contentTypes ?? [a.exportedType.identifier],
  })),
    UTImportedTypeDeclarations: [{ UTTypeIdentifier: imported, UTTypeConformsTo: ["public.data"], UTTypeTagSpecification: { "public.filename-extension": ["risum"] } }],
    UTExportedTypeDeclarations: [{ UTTypeIdentifier: exported, UTTypeConformsTo: ["public.data"], UTTypeTagSpecification: { "public.filename-extension": ["risunest"], "public.mime-type": "application/x-risunest" } }],
  };
  assert.equal(assertIosBundleMetadata(info, releaseInput, config, iosConfig).executable, "RisuNest");
  for (const key of ["UTImportedTypeDeclarations", "UTExportedTypeDeclarations"]) {
    const invalid = structuredClone(info);
    delete invalid[key];
    assert.throws(() => assertIosBundleMetadata(invalid, releaseInput, config, iosConfig), /custom document type/);
  }
  const invalid = structuredClone(info);
  delete invalid.CFBundleDocumentTypes[0].LSItemContentTypes;
  assert.throws(() => assertIosBundleMetadata(invalid, releaseInput, config, iosConfig), /bundle metadata/);
});


test("effective configuration preserves the approved desktop and mobile identities", () => {
  const base = JSON.parse(readFileSync("src-tauri/tauri.conf.json", "utf8"));
  assert.equal(base.identifier, APP_IDENTIFIER);
  for (const os of ["windows", "linux", "macos", "android", "ios"]) {
    const overlay = existsSync(`src-tauri/tauri.${os}.conf.json`)
      ? JSON.parse(readFileSync(`src-tauri/tauri.${os}.conf.json`, "utf8"))
      : null;
    assertNoIdentifierOverride(overlay, `src-tauri/tauri.${os}.conf.json`);
    assert.throws(() => assertNoIdentifierOverride({ identifier: "RisuNest" }, "synthetic"), /must not override/);
    const config = mergeTauriConfig(base, overlay, releaseTauriConfig({ product: "app", version: "1.2.3" }, "synthetic"));
    assert.equal(assertAppIdentifier(config, os), APP_IDENTIFIER);
    assert.equal(config.bundle.publisher, "Yumi");
    assert.equal(config.version, "1.2.3");
    if (os === "windows") assert.equal(config.bundle.windows.nsis.installerHooks, "windows.nsh");
    if (os === "macos") assert.equal(config.bundle.macOS.signingIdentity, "-");
    assert.throws(() => assertAppIdentifier({ ...config, identifier: "wrong" }, os), /effective/);
  }
});

test("RisuNest NSIS completes requested cleanup before removing its retry entry point", () => {
  const hook = readFileSync("src-tauri/windows.nsh", "utf8");
  assert.match(hook, /!macro NSIS_HOOK_PREUNINSTALL/);
  assert.match(hook, /\$DeleteAppDataCheckboxState = 1/);
  assert.match(hook, /\$UpdateMode <> 1/);
  assert.match(hook, /!insertmacro CheckIfAppIsRunning "\$\{MAINBINARYNAME\}\.exe" "\$\{PRODUCTNAME\}"/);
  assert.match(hook, /ExecWait '"\$INSTDIR\\\$\{MAINBINARYNAME\}\.exe" --remove-local-data --yes' \$R6/);
  assert.ok(hook.indexOf("!insertmacro CheckIfAppIsRunning") < hook.indexOf("ExecWait"));
  assert.match(hook, /\$\{If\} \$\{Errors\}\s+\$\{OrIf\} \$R6 != 0/);
  assert.match(hook, /SetErrorLevel 1\s+Abort/);
  assert.doesNotMatch(hook, /RmDir|NSIS_HOOK_POSTUNINSTALL/);
});

test("Sync GUI configuration carries one identity on every desktop platform", () => {
  const root = "server/manager/gui/src-tauri";
  const base = JSON.parse(readFileSync(`${root}/tauri.conf.json`, "utf8"));
  assert.equal(base.identifier, SYNC_IDENTIFIER);
  for (const os of ["windows", "linux", "macos"]) {
    const path = `${root}/tauri.${os}.conf.json`;
    const overlay = existsSync(path) ? JSON.parse(readFileSync(path, "utf8")) : null;
    assertNoIdentifierOverride(overlay, path);
    const config = mergeTauriConfig(base, overlay);
    assert.equal(assertSyncIdentifier(config, os), SYNC_IDENTIFIER);
    assert.throws(() => assertSyncIdentifier({ ...config, identifier: "wrong" }, os), /effective/);
  }
});

test("no owned directory overlaps the installation directory either installer selects", () => {
  const app = JSON.parse(readFileSync("src-tauri/tauri.conf.json", "utf8"));
  const sync = JSON.parse(readFileSync("server/manager/gui/src-tauri/tauri.conf.json", "utf8"));
  const syncHook = readFileSync("server/manager/install/windows.nsh", "utf8");
  const appHook = readFileSync("src-tauri/windows.nsh", "utf8");

  // The NSIS template derives $INSTDIR from PRODUCTNAME, under $LOCALAPPDATA
  // for a currentUser install. The Sync hook then rewrites it to a name
  // without spaces.
  assert.notEqual(app.bundle.windows.nsis.installMode, "perMachine");
  const bundled = JSON.parse(readFileSync("server/manager/gui/src-tauri/tauri.bundle.conf.json", "utf8"));
  assert.equal(bundled.bundle.windows.nsis.installMode, "currentUser");
  const forced = syncHook.match(/!define RISUNEST_SYNC_INSTALL_DIR "\$LOCALAPPDATA\\([^"]+)"/);
  assert.ok(forced, "the Sync hook must define its installation directory");

  const installs = {
    app: `$LOCALAPPDATA\\${app.productName}`,
    sync: `$LOCALAPPDATA\\${forced[1]}`,
  };
  const identifiers = { app: app.identifier, sync: sync.identifier };
  const known = new Set(["$INSTDIR"]);
  for (const product of ["app", "sync"]) {
    const layout = windowsLayout(product);
    // The manifest's record of the installation directory has to be the one
    // the installer actually uses, or the overlap check proves nothing.
    assert.equal(layout.install, installs[product]);
    known.add(layout.install);
    const owned = [...layout.roots, ...identifierDirectories(identifiers[product])];
    for (const root of owned) {
      assert.ok(
        !overlaps(root, layout.install),
        `${product}: ${root} overlaps ${layout.install}`,
      );
      known.add(root);
    }
    // The comparison is component-wise: RisuNestData shares a string prefix
    // with RisuNest and must still read as a sibling.
    assert.ok(overlaps(`${layout.install}\\nested`, layout.install));
  }

  // A delete-data branch may only remove a directory the manifest owns.
  for (const hook of [appHook, syncHook]) {
    for (const [, target] of hook.matchAll(/RmDir(?: \/r)? "([^"]+)"/g)) {
      assert.ok(known.has(target), `unowned RmDir target: ${target}`);
    }
  }
});

test("application bundles identify Yumi as the publisher", () => {
  const app = JSON.parse(readFileSync("src-tauri/tauri.conf.json", "utf8"));
  const sync = JSON.parse(readFileSync("server/manager/gui/src-tauri/tauri.conf.json", "utf8"));
  assert.equal(app.productName, "RisuNest");
  assert.equal(app.bundle.publisher, "Yumi");
  assert.equal(sync.productName, "RisuNest Sync");
  assert.equal(sync.bundle.publisher, "Yumi");
});

test("Sync NSIS hook separates the default install and durable data directories", () => {
  const hook = readFileSync("server/manager/install/windows.nsh", "utf8");
  assert.match(hook, /!define RISUNEST_SYNC_DEFAULT_INSTALL_DIR "\$LOCALAPPDATA\\RisuNest Sync"/);
  assert.match(hook, /!define RISUNEST_SYNC_INSTALL_DIR "\$LOCALAPPDATA\\RisuNestSync"/);
  assert.match(hook, /StrCmp \$INSTDIR "\$\{RISUNEST_SYNC_DEFAULT_INSTALL_DIR\}"/);
  assert.match(hook, /StrCpy \$INSTDIR "\$\{RISUNEST_SYNC_INSTALL_DIR\}"\s+SetOutPath \$INSTDIR/);
  assert.match(hook, /\$LOCALAPPDATA\\RisuNestSyncData\\manager-update/);
  assert.doesNotMatch(hook, /\$LOCALAPPDATA\\RisuNestSync\\manager-update/);
  assert.match(hook, /\$DeleteAppDataCheckboxState = 1/);
  assert.doesNotMatch(hook, /RmDir \/r/);
  assert.match(hook, /RISUNEST_SYNC_UNINSTALL_DATA_DIR/);
  assert.match(hook, /--data-dir "\$R9" installer delete-data/);
  assert.match(hook, /--data-dir "\$R9" --server "\$INSTDIR\\risunest-sync-server\.exe" installer forget-removal/);
  assert.match(hook, /IfFileExists "\$R9\\manager-update\\installer-\$R7\.ready"/);
  assert.ok(hook.indexOf("installer delete-data") < hook.indexOf("!macro NSIS_HOOK_POSTUNINSTALL"));
});

test("macOS package metadata and intentional ad-hoc signatures are checked", () => {
  const config = { ...tauriConfig, identifier: "io.github.rsyumi.risunest", productName: "RisuNest", mainBinaryName: "RisuNest",
    bundle: { ...tauriConfig.bundle, macOS: { minimumSystemVersion: "14.0" } },
    plugins: { "deep-link": { desktop: { schemes: ["risunestlocal"] } } },
  };
  const info = { ...iosInfo(), CFBundleIdentifier: "io.github.rsyumi.risunest", CFBundleName: "RisuNest", LSMinimumSystemVersion: "14.0" };
  assert.equal(assertMacBundleMetadata(info, releaseInput, config).identifier, "io.github.rsyumi.risunest");
  for (const key of ["CFBundleIdentifier", "CFBundleExecutable", "CFBundleName", "CFBundleShortVersionString", "CFBundleVersion", "LSMinimumSystemVersion", "CFBundleDocumentTypes", "CFBundleURLTypes"]) {
    assert.throws(() => assertMacBundleMetadata({ ...info, [key]: undefined }, releaseInput, config), /metadata/);
  }
  assert.doesNotThrow(() => assertMacAdHocSignature("Executable=RisuNest\nSignature=adhoc\nTeamIdentifier=not set\n"));
  assert.throws(() => assertMacAdHocSignature("Signature size=123\nAuthority=Unexpected\n"), /ad-hoc/);
});
