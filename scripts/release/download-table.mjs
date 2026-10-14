const rows = {
  app: [
    ["Windows", "desktop", "windows", [["nsis", "EXE"]]],
    ["macOS", "desktop", "darwin", [["dmg", "DMG"]]],
    ["Linux", "desktop", "linux", [["deb", "DEB"], ["appimage", "AppImage"]]],
    ["Android", "mobile", "android", [["apk", "APK"]]],
    ["iOS", "mobile", "ios", [["ipa", "IPA"]]],
  ],
  sync: [
    ["Windows", "managed", "windows", [["nsis", "EXE"]]],
    ["macOS", "managed", "darwin", [["dmg", "DMG"]]],
    ["Linux", "managed", "linux", [["tar.gz", "TAR.GZ"]]],
    ["Windows (raw)", "raw", "windows", [["zip", "ZIP"]]],
    ["macOS (raw)", "raw", "darwin", [["tar.gz", "TAR.GZ"]]],
    ["Linux (raw)", "raw", "linux", [["tar.gz", "TAR.GZ"]]],
  ],
};

export function renderDownloadTable({ product, downloads }, locale = "en") {
  return [
    `| ${locale === "ko" ? "플랫폼" : "Platform"} | aarch64 (arm64) | x86_64 (x64) |`,
    "| --- | --- | --- |",
    ...rows[product].map(([label, variant, os, formats]) => {
      const cells = ["aarch64", "x86_64"].map(arch => {
        if (variant === "mobile" && arch === "x86_64") return "-";
        return formats.map(([format, title]) => {
          const download = downloads.find(item => item.variant === variant && item.os === os
            && item.arch === arch && item.format === format);
          if (!download) throw new Error("release-download-missing");
          return `[${title}](${download.url})`;
        }).join(" / ");
      });
      return `| ${label} | ${cells.join(" | ")} |`;
    }),
  ].join("\n");
}
