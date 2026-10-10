import { readFileSync, readdirSync, statSync } from "node:fs";
import { resolve } from "node:path";
import { spawnSync } from "node:child_process";

export function writeZipArchive(source, archive) {
  const entries = readdirSync(source).sort();
  if (!entries.length) throw new Error("Package staging directory is empty.");
  const child = spawnSync("python", ["-m", "zipfile", "-c", resolve(archive), ...entries], {
    cwd: source,
    stdio: "inherit",
    windowsHide: true,
  });
  if (child.error) throw child.error;
  if (child.status !== 0) throw new Error(`ZIP creation failed (${child.status}).`);
}

export function assertPackageFormat(path, format) {
  const stats = statSync(path);
  if (!stats.isFile() || stats.size === 0) throw new Error(`${path} is empty.`);
  const bytes = readFileSync(path);
  const starts = (...values) => values.every((value, index) => bytes[index] === value);
  if (["zip", "apk", "ipa"].includes(format) && !starts(0x50, 0x4b))
    throw new Error(`${path} is not a ZIP container.`);
  if (format === "nsis" && !starts(0x4d, 0x5a))
    throw new Error(`${path} is not a Windows executable.`);
  if (format === "deb" && bytes.toString("ascii", 0, 8) !== "!<arch>\n")
    throw new Error(`${path} is not a Debian archive.`);
  if (format === "appimage" && !starts(0x7f, 0x45, 0x4c, 0x46))
    throw new Error(`${path} is not an ELF AppImage.`);
  if (["tar.gz", "app.tar.gz"].includes(format) && !starts(0x1f, 0x8b))
    throw new Error(`${path} is not gzip-compressed.`);
  if (format === "dmg" && bytes.subarray(-512, -508).toString("ascii") !== "koly")
    throw new Error(`${path} is not a UDIF disk image.`);
  return { format, size: stats.size };
}
