import hashlib
import json
import os
from pathlib import Path
import plistlib
import sys
import zipfile


def comparison_bytes(relative, data):
    if relative.endswith("_iOS/Info.plist"):
        info = plistlib.loads(data)
        for key in ("CFBundleVersion", "CFBundleDocumentTypes", "CFBundleURLTypes"):
            info.pop(key, None)
        return plistlib.dumps(info, fmt=plistlib.FMT_XML, sort_keys=True)
    if relative.endswith(".entitlements"):
        return plistlib.dumps(plistlib.loads(data), fmt=plistlib.FMT_XML, sort_keys=True)
    if relative.endswith(".xcodeproj/project.pbxproj"):
        return data.replace(b'PRODUCT_NAME = "RisuNest";', b"PRODUCT_NAME = RisuNest;")
    return data


def archive_inputs(root, output):
    root, output = Path(root), Path(output)
    if not root.is_dir():
        raise ValueError("Missing Apple shell")
    output.mkdir(parents=True, exist_ok=True)
    excluded = {"build", "target", "Externals", "externals", "xcuserdata", ".build"}
    records = []
    with zipfile.ZipFile(output / "apple-shell-inputs.zip", "w", zipfile.ZIP_DEFLATED) as archive:
        for directory, dirs, files in os.walk(root, followlinks=False):
            dirs[:] = sorted(d for d in dirs if d not in excluded and not (Path(directory) / d).is_symlink())
            for name in sorted(files):
                path = Path(directory) / name
                if path.is_symlink() or name == ".DS_Store" or name.endswith((".xcuserstate", ".a", ".dylib")):
                    continue
                relative = path.relative_to(root).as_posix()
                data = path.read_bytes()
                comparison = comparison_bytes(relative, data)
                records.append({
                    "path": relative,
                    "rawBytes": len(data),
                    "rawSha256": hashlib.sha256(data).hexdigest(),
                    "comparisonBytes": len(comparison),
                    "comparisonSha256": hashlib.sha256(comparison).hexdigest(),
                })
                archive.writestr(relative, data)
    if not any(r["path"].endswith("project.pbxproj") for r in records):
        raise ValueError("Missing generated Xcode project")
    (output / "apple-shell-inputs.json").write_text(json.dumps(records, indent=2) + "\n")


def compare_inputs(before_path, after_path, output_path):
    before = {record["path"]: record for record in json.loads(Path(before_path).read_text())}
    after = {record["path"]: record for record in json.loads(Path(after_path).read_text())}
    if before.keys() != after.keys():
        raise ValueError("Apple shell file set changed during the iOS build")
    changed = [
        path for path in before
        if (before[path]["comparisonBytes"], before[path]["comparisonSha256"])
        != (after[path]["comparisonBytes"], after[path]["comparisonSha256"])
    ]
    if changed:
        raise ValueError(f"Apple shell inputs changed during the iOS build: {changed}")
    raw_changed = [
        path for path in before
        if (before[path]["rawBytes"], before[path]["rawSha256"])
        != (after[path]["rawBytes"], after[path]["rawSha256"])
    ]
    Path(output_path).write_text(json.dumps({
        "schema": "risunest-apple-shell-preservation/v1",
        "preserved": True,
        "files": len(before),
        "tauriManagedRewrites": raw_changed,
    }, indent=2) + "\n")


if __name__ == "__main__":
    if len(sys.argv) == 5 and sys.argv[1] == "compare":
        compare_inputs(*sys.argv[2:])
    else:
        archive_inputs(*sys.argv[1:])
