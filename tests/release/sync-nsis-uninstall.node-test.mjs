import assert from 'node:assert/strict';
import { existsSync, mkdtempSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { dirname, join, resolve } from 'node:path';
import { spawnSync } from 'node:child_process';
import test from 'node:test';

const compiler = process.env.NSIS_MAKENSIS ?? join(process.env.LOCALAPPDATA ?? '', 'tauri', 'NSIS', 'makensis.exe');
const installedExecutable = '$INSTDIR\\${MAINBINARYNAME}.exe';
test('Sync removal hooks compile in the uninstaller context', { skip: process.platform !== 'win32' || !existsSync(compiler) }, () => {
  const directory = mkdtempSync(join(tmpdir(), 'risunest-sync-nsis-'));
  try {
    const script = join(directory, 'test.nsi');
    writeFileSync(script, String.raw`
Unicode true
RequestExecutionLevel user
OutFile "${join(directory, 'test.exe')}"
!include LogicLib.nsh
!include FileFunc.nsh
!define MAINBINARYNAME "SyntheticSync"
!define PRODUCTNAME "Synthetic Sync"
!macro CheckIfAppIsRunning executable product
!macroend
Var UpdateMode
Var DeleteAppDataCheckboxState
!include "${resolve('server/manager/install/windows.nsh')}"
Section
WriteUninstaller "$INSTDIR\uninstall.exe"
SectionEnd
Section Uninstall
!insertmacro NSIS_HOOK_PREUNINSTALL
!insertmacro NSIS_HOOK_POSTUNINSTALL
SectionEnd
`);
    const result = spawnSync(compiler, ['/V2', script], { encoding: 'utf8', timeout: 30000, windowsHide: true });
    assert.equal(result.status, 0, `${result.stdout}\n${result.stderr}`);
  } finally { rmSync(directory, { recursive: true, force: true }); }
});
import { readFileSync, mkdirSync } from 'node:fs';

test('Sync NSIS preserves data by default, protects updates, and aborts before binary removal on cleanup failure', { skip: process.platform !== 'win32' || !existsSync(compiler) }, () => {
  const directory = mkdtempSync(join(tmpdir(), 'risunest-sync-nsis-runtime-'));
  const q = value => value.replaceAll('$', '$$').replaceAll('"', '$\\"');
  try {
    const stubSource = join(directory, 'manager.rs');
    writeFileSync(stubSource, String.raw`
use std::{env, fs, io::Write};
fn main() {
 let root=env::current_exe().unwrap().parent().unwrap().to_path_buf();
 let mut values=env::args().skip(1).collect::<Vec<_>>();
 let data=if values.first().is_some_and(|v| v=="--data-dir") {let data=std::path::PathBuf::from(values[1].clone());values.drain(0..2);data} else {root.join("local/RisuNestSyncData")};
 if values.iter().any(|v| v=="forget-removal") {return;}
 assert!(data.starts_with(&root));
 let args=values.join(" ");
 let mut log=fs::OpenOptions::new().create(true).append(true).open(root.join("calls")).unwrap();
 writeln!(log,"{args}: {}",data.display()).unwrap();
 if values.first().is_some_and(|v| v=="installer") && values.get(1).is_some_and(|v| v=="prepare") {
  assert_eq!(values.len(),3);
  let owner=values[2].parse::<u32>().unwrap();
  assert!(owner>0 && owner!=std::process::id());
  println!("aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa");
 }
 if args=="installer delete-data" {
  if root.join("fail").exists() {std::process::exit(9);}
  fs::remove_dir_all(data).unwrap();
 }
}
`);
    const binary = join(directory, 'manager.exe');
    const built = spawnSync('rustc', ['--edition=2021', stubSource, '-o', binary], { encoding: 'utf8', timeout: 60000, windowsHide: true });
    assert.equal(built.status, 0, built.stderr);
    const hook = join(directory, 'hook.nsh');
    writeFileSync(hook, readFileSync(resolve('server/manager/install/windows.nsh'), 'utf8').replaceAll('$LOCALAPPDATA', '$INSTDIR\\local'));
    const script = join(directory, 'installer.nsi');
    writeFileSync(script, String.raw`
Unicode true
RequestExecutionLevel user
SilentInstall silent
SilentUnInstall silent
OutFile "${q(join(directory, 'installer.exe'))}"
!include LogicLib.nsh
!include FileFunc.nsh
!define MAINBINARYNAME "SyntheticSync"
!define PRODUCTNAME "Synthetic Sync"
Var UpdateMode
Var DeleteAppDataCheckboxState
!macro CheckIfAppIsRunning executable product
  StrCmp "${'$'}{executable}" "${installedExecutable}" +3
  SetErrorLevel 9
  Abort "Process check must use the installed executable path"
!macroend
!include "${q(hook)}"
Section
SetOutPath "$INSTDIR"
File /oname=risunest-sync-manager.exe "${q(binary)}"
WriteUninstaller "$INSTDIR\uninstall.exe"
SectionEnd
Function un.onInit
${'$'}{GetParameters} $0
ClearErrors
${'$'}{GetOptions} $0 "/UPDATE" $1
${'$'}{IfNot} ${'$'}{Errors}
StrCpy $UpdateMode 1
${'$'}{EndIf}
FunctionEnd
Section Uninstall
!insertmacro NSIS_HOOK_PREUNINSTALL
Delete "$INSTDIR\risunest-sync-manager.exe"
!insertmacro NSIS_HOOK_POSTUNINSTALL
SectionEnd
`);
    const compile = spawnSync(compiler, ['/V2', script], { encoding: 'utf8', timeout: 30000, windowsHide: true });
    assert.equal(compile.status, 0, `${compile.stdout}\n${compile.stderr}`);
    for (const scenario of [
      { name: 'preserve', args: [], data: true, installed: false, code: 0 },
      { name: 'delete', args: ['/DELETEAPPDATA'], data: false, installed: false, code: 0 },
      { name: 'custom', custom: true, args: ['/DELETEAPPDATA'], data: false, installed: false, code: 0 },
      { name: 'update', args: ['/UPDATE', '/DELETEAPPDATA'], data: true, installed: false, code: 0 },
      { name: 'failed', args: ['/DELETEAPPDATA'], fail: true, data: true, installed: true, code: 1 },
    ]) {
      const location = join(directory, scenario.name);
      const dataRoot = join(location, scenario.custom ? 'custom server data' : 'local/RisuNestSyncData');
      mkdirSync(dataRoot, { recursive: true });
      writeFileSync(join(dataRoot, 'synthetic'), 'fixture');
      writeFileSync(join(dataRoot, 'risunest-sync-instance.json'), '{}');
      if (scenario.fail) writeFileSync(join(location, 'fail'), 'fixture');
      assert.equal(spawnSync(join(directory, 'installer.exe'), ['/S', `/D=${location}`], { timeout: 30000, windowsHide: true }).status, 0);
      const result = spawnSync(join(location, 'uninstall.exe'), ['/S', ...scenario.args, `_?=${location}`], { timeout: 30000, windowsHide: true, env: { ...process.env, RISUNEST_SYNC_UNINSTALL_DATA_DIR: scenario.custom ? dataRoot : '' } });
      assert.equal(result.status, scenario.code, `${scenario.name}: exit ${existsSync(join(location, "calls")) ? readFileSync(join(location, "calls"), "utf8") : "no manager calls"}`);
      assert.equal(existsSync(join(location, 'risunest-sync-manager.exe')), scenario.installed, `${scenario.name}: manager remains on failure`);
      assert.equal(existsSync(join(dataRoot, 'synthetic')), scenario.data, `${scenario.name}: data`);
      const calls = readFileSync(join(location, 'calls'), 'utf8');
      if (scenario.name === 'update') {
        assert.match(calls, /^prepare-update:/);
        assert.doesNotMatch(calls, /uninstall|delete-data|forget-removal/);
      } else {
        assert.equal([...calls.matchAll(/^installer prepare ([1-9]\d*):/gm)].length, 1, `${scenario.name}: prepare receives the uninstaller owner PID`);
        assert.match(calls, /^uninstall lock-held:/m);
        assert.ok(calls.indexOf('installer prepare ') < calls.indexOf('uninstall lock-held'), `${scenario.name}: acquire guard before cleanup`);
      }
      if (scenario.name === 'delete') {
        assert.match(calls, /^installer finish aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa:/m);
        assert.ok(calls.indexOf('installer finish') < calls.indexOf('installer delete-data'));
      }
    }
  } finally { rmSync(directory, { recursive: true, force: true }); }
});

test('Sync preinstall checks the resolved installation directory', { skip: process.platform !== 'win32' || !existsSync(compiler) }, () => {
  const directory = mkdtempSync(join(tmpdir(), 'risunest-sync-nsis-path-'));
  const q = value => value.replaceAll('$', '$$').replaceAll('"', '$\\"');
  const original = join(directory, 'original');
  const normalized = join(directory, 'normalized');
  const custom = join(directory, 'custom');
  try {
    const script = join(directory, 'installer.nsi');
    writeFileSync(script, String.raw`
Unicode true
RequestExecutionLevel user
SilentInstall silent
OutFile "${q(join(directory, 'installer.exe'))}"
!include LogicLib.nsh
!include FileFunc.nsh
!define MAINBINARYNAME "SyntheticSync"
!define PRODUCTNAME "Synthetic Sync"
!define RISUNEST_SYNC_DEFAULT_INSTALL_DIR "${q(original)}"
!define RISUNEST_SYNC_INSTALL_DIR "${q(normalized)}"
!macro CheckIfAppIsRunning executable product
  FileOpen $0 "$EXEDIR\checked-path" w
  FileWrite $0 "${'$'}{executable}"
  FileClose $0
!macroend
!include "${q(resolve('server/manager/install/windows.nsh'))}"
Section
  !insertmacro NSIS_HOOK_PREINSTALL
SectionEnd
`);
    const compiled = spawnSync(compiler, ['/V2', script], { encoding: 'utf8', timeout: 30000, windowsHide: true });
    assert.equal(compiled.status, 0, `${compiled.stdout}\n${compiled.stderr}`);
    for (const [requested, expected] of [[original, normalized], [custom, custom]]) {
      const result = spawnSync(join(directory, 'installer.exe'), ['/S', `/D=${requested}`], { timeout: 30000, windowsHide: true });
      assert.equal(result.status, 0);
      assert.equal(readFileSync(join(directory, 'checked-path'), 'utf8'), join(expected, 'SyntheticSync.exe'));
    }
  } finally {
    assert.equal(dirname(directory), resolve(tmpdir()));
    rmSync(directory, { recursive: true, force: true, maxRetries: 10, retryDelay: 100 });
  }
});
