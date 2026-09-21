import assert from "node:assert/strict";
import { existsSync, mkdirSync, mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join, resolve } from "node:path";
import { spawnSync } from "node:child_process";
import test from "node:test";

const compiler = process.env.NSIS_MAKENSIS ?? join(process.env.LOCALAPPDATA ?? "", "tauri", "NSIS", "makensis.exe");
const skip = process.platform !== "win32" ? "NSIS execution requires Windows" : !existsSync(compiler) ? "NSIS compiler is unavailable" : false;
const quote = (value) => value.replaceAll("$", "$$").replaceAll('"', '$\\"');

test("NSIS executes app cleanup only on explicit removal and retains retry capability on failure", { skip }, () => {
  const root = mkdtempSync(join(tmpdir(), "risunest-nsis-contract-"));
  const compile = (name, source) => {
    const script = join(root, `${name}.nsi`);
    writeFileSync(script, source);
    const result = spawnSync(compiler, ["/V2", script], { encoding: "utf8", timeout: 30_000 });
    assert.equal(result.status, 0, `${result.stdout}\n${result.stderr}`);
  };
  try {
    compile("cleanup", String.raw`
Unicode true
RequestExecutionLevel user
SilentInstall silent
OutFile "${quote(join(root, "cleanup.exe"))}"
!include FileFunc.nsh
Section
  ${"$"}{GetParameters} $0
  StrCmp $0 "--remove-local-data --yes" 0 invalid
  IfFileExists "$EXEDIR\process-checked" 0 invalid
  FileOpen $0 "$EXEDIR\cleanup-called" w
  FileWrite $0 "called"
  FileClose $0
  IfFileExists "$EXEDIR\fail-cleanup" invalid 0
  Delete "$EXEDIR\synthetic-data"
  SetErrorLevel 0
  Quit
  invalid:
  SetErrorLevel 7
SectionEnd
`);
    compile("installer", String.raw`
Unicode true
RequestExecutionLevel user
SilentInstall silent
SilentUnInstall silent
OutFile "${quote(join(root, "installer.exe"))}"
!include LogicLib.nsh
!include FileFunc.nsh
!define MAINBINARYNAME "SyntheticRisuNest"
!define PRODUCTNAME "Synthetic RisuNest"
Var DeleteAppDataCheckboxState
Var UpdateMode
!macro CheckIfAppIsRunning executableName productName
  FileOpen $0 "$INSTDIR\process-checked" w
  FileWrite $0 "checked"
  FileClose $0
!macroend
!include "${quote(resolve("src-tauri/windows.nsh"))}"
Section
  SetOutPath "$INSTDIR"
  File /oname=SyntheticRisuNest.exe "${quote(join(root, "cleanup.exe"))}"
  WriteUninstaller "$INSTDIR\uninstall.exe"
  FileOpen $0 "$INSTDIR\registered" w
  FileWrite $0 "registered"
  FileClose $0
SectionEnd
Function un.onInit
  ${"$"}{GetParameters} $0
  ClearErrors
  ${"$"}{GetOptions} $0 "/DELETE_DATA" $1
  ${"$"}{IfNot} ${"$"}{Errors}
    StrCpy $DeleteAppDataCheckboxState 1
  ${"$"}{EndIf}
  ClearErrors
  ${"$"}{GetOptions} $0 "/UPDATE" $1
  ${"$"}{IfNot} ${"$"}{Errors}
    StrCpy $UpdateMode 1
  ${"$"}{EndIf}
FunctionEnd
Section Uninstall
  !insertmacro NSIS_HOOK_PREUNINSTALL
  !insertmacro CheckIfAppIsRunning "${"$"}{MAINBINARYNAME}.exe" "${"$"}{PRODUCTNAME}"
  Delete "$INSTDIR\SyntheticRisuNest.exe"
  Delete "$INSTDIR\registered"
SectionEnd
`);
    for (const scenario of [
      { name: "default", args: [], data: true, called: false, installed: false, status: 0 },
      { name: "explicit", args: ["/DELETE_DATA"], data: false, called: true, installed: false, status: 0 },
      { name: "update", args: ["/UPDATE", "/DELETE_DATA"], data: true, called: false, installed: false, status: 0 },
      { name: "failed", args: ["/DELETE_DATA"], fail: true, data: true, called: true, installed: true, status: 1 },
      { name: "missing", args: ["/DELETE_DATA"], missing: true, data: true, called: false, installed: true, status: 1 },
    ]) {
      const location = join(root, scenario.name);
      mkdirSync(location);
      const install = spawnSync(join(root, "installer.exe"), ["/S", `/D=${location}`], { timeout: 30_000 });
      assert.equal(install.status, 0, scenario.name);
      writeFileSync(join(location, "synthetic-data"), "fixture");
      if (scenario.fail) writeFileSync(join(location, "fail-cleanup"), "fixture");
      if (scenario.missing) rmSync(join(location, "SyntheticRisuNest.exe"));
      const uninstall = spawnSync(join(location, "uninstall.exe"), ["/S", ...scenario.args, `_?=${location}`], { timeout: 30_000 });
      assert.equal(uninstall.status, scenario.status, `${scenario.name}: exit`);
      assert.equal(existsSync(join(location, "synthetic-data")), scenario.data, `${scenario.name}: data`);
      assert.equal(existsSync(join(location, "cleanup-called")), scenario.called, `${scenario.name}: cleanup invocation`);
      assert.equal(existsSync(join(location, "registered")), scenario.installed, `${scenario.name}: registration`);
      if (!scenario.missing) assert.equal(existsSync(join(location, "SyntheticRisuNest.exe")), scenario.installed, `${scenario.name}: binary`);
      if (scenario.fail) {
        rmSync(join(location, "fail-cleanup"));
        const retry = spawnSync(join(location, "uninstall.exe"), ["/S", "/DELETE_DATA", `_?=${location}`], { timeout: 30_000 });
        assert.equal(retry.status, 0, "retry succeeds");
        assert.equal(existsSync(join(location, "synthetic-data")), false);
        assert.equal(existsSync(join(location, "registered")), false);
      }
    }
  } finally {
    assert.equal(dirname(root), resolve(tmpdir()));
    rmSync(root, { recursive: true, force: true });
  }
});
