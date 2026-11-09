"""Build one pinned standalone Lua guest using an explicitly supplied SDK."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import subprocess
import sys
import tarfile
import tempfile

LUA_VERSION = "5.4.9"
LUA_SHA256 = "2335b6c582a52654f94612bf10d2f4672805d05329aa6568b1d8cd9e5c6fb8e6"
EMSCRIPTEN_VERSION = "6.0.13"
EMSDK_RELEASE = "eb132757325583b787fe9414b09afea9442b72ad"
SOURCES = "lapi lcode lctype ldebug ldo ldump lfunc lgc llex lmem lobject lopcodes lparser lstate lstring ltable ltm lundump lvm lzio lauxlib lbaselib lcorolib lmathlib lstrlib ltablib lutf8lib".split()
EXPORTS = ["probe_input_ptr", "probe_input_capacity", "probe_result", "probe_pending", "probe_init", "probe_load", "probe_resume", "probe_callback"]


def digest(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--sdk", type=Path, required=True)
    parser.add_argument("--lua-archive", type=Path, required=True)
    parser.add_argument("--out", type=Path, required=True)
    args = parser.parse_args()
    sdk, archive, output = args.sdk.resolve(), args.lua_archive.resolve(), args.out.resolve()
    compiler = sdk / "upstream/emscripten/emcc.py"
    configuration = sdk / ".emscripten"
    version_file = sdk / "upstream/emscripten/emscripten-version.txt"
    if not compiler.is_file() or not configuration.is_file():
        raise SystemExit("prerequisite-missing: explicitly supplied SDK must be installed and activated locally")
    if version_file.read_text().strip().strip('"') != EMSCRIPTEN_VERSION:
        raise SystemExit("prerequisite-mismatch: Emscripten version")
    tags = json.loads((sdk / "emscripten-releases-tags.json").read_text())
    if tags["releases"].get(EMSCRIPTEN_VERSION) != EMSDK_RELEASE:
        raise SystemExit("prerequisite-mismatch: SDK release manifest")
    if digest(archive) != LUA_SHA256:
        raise SystemExit("prerequisite-mismatch: Lua source archive digest")
    if output.suffix != ".wasm":
        raise SystemExit("output must end in .wasm")
    output.parent.mkdir(parents=True, exist_ok=True)
    bridge = Path(__file__).with_name("bridge.c").resolve()
    options = ["-O2", "-sSUPPORT_LONGJMP=wasm", "-sWASM_LEGACY_EXCEPTIONS=0",
               "-sSTANDALONE_WASM=1", "--no-entry", "-sINITIAL_MEMORY=33554432",
               "-sALLOW_MEMORY_GROWTH=0", "-sABORTING_MALLOC=0",
               "-sSTACK_SIZE=1048576", "-sFILESYSTEM=0", "-sASSERTIONS=0",
               "-sEXPORTED_FUNCTIONS=" + json.dumps(["_" + name for name in EXPORTS])]
    temporary_root = output.parent / "tmp"
    temporary_root.mkdir(exist_ok=True)
    # Avoid expanded system-library batches exceeding Windows command-line limits.
    environment = dict(os.environ, EM_CONFIG=str(configuration), EM_CACHE=str(sdk / "upstream/emscripten/cache"),
                       EMCC_CORES="1", EMCC_BATCH_BUILD="0", BINARYEN_CORES="1", TMP=str(temporary_root),
                       TEMP=str(temporary_root), TMPDIR=str(temporary_root))
    # Verified archive contents retain Lua's original copyright and license notices.
    with tempfile.TemporaryDirectory(prefix="lua-guest-", dir=output.parent) as temporary:
        source = Path(temporary)
        with tarfile.open(archive, "r:gz") as tar:
            for member in tar.getmembers():
                prefix = "lua-" + LUA_VERSION + "/src/"
                if member.name.startswith(prefix) and member.isfile():
                    basename = member.name[len(prefix):]
                    if "/" in basename or "\\" in basename or basename in ("", ".", ".."):
                        raise SystemExit("unexpected source archive path")
                    with tar.extractfile(member) as original:
                        (source / basename).write_bytes(original.read())
        command = [sys.executable, str(compiler), *options, "-I", str(source), str(bridge),
                   *[str(source / (name + ".c")) for name in SOURCES], "-o", str(output)]
        subprocess.run(command, env=environment, check=True)
    receipt = {"schema": 1, "lua": LUA_VERSION, "luaArchiveSha256": LUA_SHA256,
               "emscripten": EMSCRIPTEN_VERSION, "emsdkRelease": EMSDK_RELEASE,
               "bridgeSha256": digest(bridge), "builderSha256": digest(Path(__file__)),
               "wasmSha256": digest(output), "cacheKind": "pinned-sdk", "options": options}
    output.with_suffix(".receipt.json").write_text(json.dumps(receipt, indent=2) + "\n", encoding="utf-8")
    print(json.dumps(receipt))


if __name__ == "__main__":
    main()
