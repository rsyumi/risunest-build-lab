import hashlib
import json
import os
from pathlib import Path
import sys
import zipfile


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
                records.append({"path": relative, "bytes": len(data), "sha256": hashlib.sha256(data).hexdigest()})
                archive.writestr(relative, data)
    if not any(r["path"].endswith("project.pbxproj") for r in records):
        raise ValueError("Missing generated Xcode project")
    (output / "apple-shell-inputs.json").write_text(json.dumps(records, indent=2) + "\n")


if __name__ == "__main__":
    archive_inputs(*sys.argv[1:])
