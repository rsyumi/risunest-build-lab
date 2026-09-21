import assert from 'node:assert/strict';
import { existsSync, mkdtempSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join, resolve } from 'node:path';
import { spawnSync } from 'node:child_process';
import test from 'node:test';

const compiler = process.env.NSIS_MAKENSIS ?? join(process.env.LOCALAPPDATA ?? '', 'tauri', 'NSIS', 'makensis.exe');
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
    const result = spawnSync(compiler, ['/V2', script], { encoding: 'utf8', timeout: 30000 });
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
 if args=="installer prepare" {println!("{}","a".repeat(64));}
 if args=="installer delete-data" {
  if root.join("fail").exists() {std::process::exit(9);}
  fs::remove_dir_all(data).unwrap();
 }
}
`);
    const binary = join(directory, 'manager.exe');
    const built = spawnSync('rustc', ['--edition=2021', stubSource, '-o', binary], { encoding: 'utf8', timeout: 60000 });
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
    const compile = spawnSync(compiler, ['/V2', script], { encoding: 'utf8', timeout: 30000 });
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
      assert.equal(spawnSync(join(directory, 'installer.exe'), ['/S', `/D=${location}`], { timeout: 30000 }).status, 0);
      const result = spawnSync(join(location, 'uninstall.exe'), ['/S', ...scenario.args, `_?=${location}`], { timeout: 30000, env: { ...process.env, RISUNEST_SYNC_UNINSTALL_DATA_DIR: scenario.custom ? dataRoot : '' } });
      assert.equal(result.status, scenario.code, `${scenario.name}: exit ${existsSync(join(location, "calls")) ? readFileSync(join(location, "calls"), "utf8") : "no manager calls"}`);
      assert.equal(existsSync(join(location, 'risunest-sync-manager.exe')), scenario.installed, `${scenario.name}: manager remains on failure`);
      assert.equal(existsSync(join(dataRoot, 'synthetic')), scenario.data, `${scenario.name}: data`);
      if (scenario.name === 'update') {
        const calls = readFileSync(join(location, 'calls'), 'utf8');
        assert.match(calls, /^prepare-update:/);
        assert.doesNotMatch(calls, /uninstall|delete-data|forget-removal/);
      }
      if (scenario.name === 'delete') {
        const calls = readFileSync(join(location, 'calls'), 'utf8');
        assert.ok(calls.indexOf('installer finish') < calls.indexOf('installer delete-data'));
      }
    }
  } finally { rmSync(directory, { recursive: true, force: true }); }
});
