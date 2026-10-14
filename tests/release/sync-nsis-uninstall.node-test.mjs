import assert from 'node:assert/strict';
import { existsSync, mkdtempSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { dirname, join, resolve } from 'node:path';
import { spawnSync } from 'node:child_process';
import test from 'node:test';

const compiler = process.env.NSIS_MAKENSIS ?? join(process.env.LOCALAPPDATA ?? '', 'tauri', 'NSIS', 'makensis.exe');
const skip = process.platform !== 'win32' || !existsSync(compiler);
if (process.env.RISUNEST_REQUIRE_NSIS === '1' && skip) throw new Error('NSIS compiler is required');
const installedExecutable = '$INSTDIR\\${MAINBINARYNAME}.exe';
test('Sync removal hooks compile in the uninstaller context', { skip }, () => {
  const directory = mkdtempSync(join(tmpdir(), 'risunest-sync-nsis-'));
  try {
    const script = join(directory, 'test.nsi');
    writeFileSync(script, String.raw`
Unicode true
RequestExecutionLevel user
OutFile "${join(directory, 'test.exe')}"
!include LogicLib.nsh
!include FileFunc.nsh
LoadLanguageFile "${'$'}{NSISDIR}\Contrib\Language files\English.nlf"
LoadLanguageFile "${'$'}{NSISDIR}\Contrib\Language files\Korean.nlf"
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
import { readFileSync, mkdirSync, copyFileSync } from 'node:fs';

const q = value => value.replaceAll('$', '$$').replaceAll('"', '$\\"');
function buildManagerStub(directory) {
  const stubSource = join(directory, 'manager.rs');
  writeFileSync(stubSource, String.raw`
use std::{env, fs, io::Write};
fn main() {
 let mut root=env::current_exe().unwrap().parent().unwrap().to_path_buf();
 let mut values=env::args().skip(1).collect::<Vec<_>>();
 let data=if values.first().is_some_and(|v| v=="--data-dir") {let data=std::path::PathBuf::from(values[1].clone());values.drain(0..2);data} else {root.join("local/RisuNestSyncData")};
 if values.first().is_some_and(|v| v=="--server") {root=std::path::PathBuf::from(&values[1]).parent().unwrap().to_owned();values.drain(0..2);}
 if values.iter().any(|v| v=="forget-removal") {return;}
 assert!(data.starts_with(&root));
 let args=values.join(" ").trim().to_owned();
 if args=="installer removal-check normal delete" {println!("delete-data");}
 if args=="installer removal-check normal preserve" {println!("keep-data");}
 if args=="installer removal-check force delete" {println!("keep-data");}
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
  if data.exists() {fs::remove_dir_all(data).unwrap();}
 }
 if values.first().is_some_and(|v| v=="installer") && values.get(1).is_some_and(|v| v=="remove-files") && root.join("fail-remove").exists() {
  fs::remove_file(root.join("fail-remove")).unwrap();
  println!("removal-file-failed: synthetic");
  std::process::exit(7);
 }
 if args=="installer register" && root.join("fail-register").exists() {
  fs::remove_file(root.join("fail-register")).unwrap();
  println!("synthetic-register-failure");
  std::process::exit(7);
 }
 if values.len()==3 && values[0]=="installer" && values[1]=="obsolete-files" {
  fs::write(root.join("baseline-received"), fs::read(&values[2]).unwrap_or_else(|_| b"missing".to_vec())).unwrap();
 }
}
`);
  const binary = join(directory, 'manager.exe');
  const built = spawnSync('rustc', ['--edition=2021', stubSource, '-o', binary], { encoding: 'utf8', timeout: 60000, windowsHide: true });
  assert.equal(built.status, 0, built.stderr);
  return binary;
}
function writeManagerHook(directory) {
  const hook = join(directory, 'hook.nsh');
  writeFileSync(hook, readFileSync(resolve('server/manager/install/windows.nsh'), 'utf8').replaceAll('$LOCALAPPDATA', '$INSTDIR\\local').replace(/!include [^\r\n]+windows-common.nsh"/, `!include "${resolve('src-tauri/windows-common.nsh')}"`));
  return hook;
}

test('Sync NSIS preserves data by default, protects updates, and aborts before binary removal on cleanup failure', { skip }, () => {
  const directory = mkdtempSync(join(tmpdir(), 'risunest-sync-nsis-runtime-'));
  try {
    const binary = buildManagerStub(directory);
    const hook = writeManagerHook(directory);
    const script = join(directory, 'installer.nsi');
    writeFileSync(script, String.raw`
Unicode true
RequestExecutionLevel user
SilentInstall silent
SilentUnInstall silent
OutFile "${q(join(directory, 'installer.exe'))}"
!include LogicLib.nsh
!include FileFunc.nsh
LoadLanguageFile "${'$'}{NSISDIR}\Contrib\Language files\English.nlf"
LoadLanguageFile "${'$'}{NSISDIR}\Contrib\Language files\Korean.nlf"
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
Function un.onUninstFailed
FileOpen $0 "$INSTDIR\diagnostic" w
FileWrite $0 "$SyncFailureMessage"
FileClose $0
FunctionEnd
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
; The template preselects deletion for /DELETEAPPDATA; /UNCHECKED stands for the user clearing it.
ClearErrors
${'$'}{GetOptions} $0 "/DELETEAPPDATA" $1
${'$'}{IfNot} ${'$'}{Errors}
StrCpy $DeleteAppDataCheckboxState 1
${'$'}{EndIf}
ClearErrors
${'$'}{GetOptions} $0 "/UNCHECKED" $1
${'$'}{IfNot} ${'$'}{Errors}
StrCpy $DeleteAppDataCheckboxState 0
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
      { name: 'forced-unregistered', args: ['/FORCE', '/DELETEAPPDATA'], unregistered: true, data: true, installed: false, code: 0 },
      { name: 'unchecked', args: ['/DELETEAPPDATA', '/UNCHECKED'], data: true, installed: false, code: 0 },
      { name: 'retry', args: ['/DELETEAPPDATA'], failRemove: true, data: false, installed: true, code: 1 },
    ]) {
      const location = join(directory, scenario.name);
      const dataRoot = join(location, scenario.custom ? 'custom server data' : 'local/RisuNestSyncData');
      mkdirSync(dataRoot, { recursive: true });
      writeFileSync(join(dataRoot, 'synthetic'), 'fixture');
      if (!scenario.unregistered) writeFileSync(join(dataRoot, 'risunest-sync-instance.json'), '{}');
      if (scenario.fail) writeFileSync(join(location, 'fail'), 'fixture');
      if (scenario.failRemove) writeFileSync(join(location, 'fail-remove'), 'fixture');
      assert.equal(spawnSync(join(directory, 'installer.exe'), ['/S', `/D=${location}`], { timeout: 30000, windowsHide: true }).status, 0);
      const uninstall = () => spawnSync(join(location, 'uninstall.exe'), ['/S', ...scenario.args, `_?=${location}`], { timeout: 30000, windowsHide: true, env: { ...process.env, RISUNEST_SYNC_UNINSTALL_DATA_DIR: scenario.custom ? dataRoot : '' } });
      const result = uninstall();
      assert.equal(result.status, scenario.code, `${scenario.name}: exit ${existsSync(join(location, "calls")) ? readFileSync(join(location, "calls"), "utf8") : "no manager calls"}`);
      assert.equal(existsSync(join(location, 'risunest-sync-manager.exe')), scenario.installed, `${scenario.name}: manager remains on failure`);
      assert.equal(existsSync(join(dataRoot, 'synthetic')), scenario.data, `${scenario.name}: data`);
      const calls = readFileSync(join(location, 'calls'), 'utf8');
      if (scenario.name === 'update') {
        assert.match(calls, /^prepare-update:/);
        assert.doesNotMatch(calls, /uninstall|delete-data|forget-removal/);
      } else {
        assert.equal([...calls.matchAll(/^installer prepare ([1-9]\d*):/gm)].length, 1, `${scenario.name}: prepare receives the uninstaller owner PID`);
        assert.match(calls, /^uninstall lock-held(?: --force)?:/m);
        assert.ok(calls.indexOf('installer prepare ') < calls.indexOf('uninstall lock-held'), `${scenario.name}: acquire guard before cleanup`);
      }
      if (scenario.name === 'delete') {
        assert.match(calls, /^installer finish aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa:/m);
        assert.ok(calls.indexOf('installer finish') < calls.indexOf('installer delete-data'));
      }
      if (scenario.unregistered) {
        assert.match(calls, /installer removal-check force delete/);
        assert.match(calls, /uninstall lock-held --force/);
        assert.match(calls, /installer remove-files force/);
        assert.doesNotMatch(calls, /installer delete-data/);
      }
      if (scenario.name === 'unchecked') {
        assert.match(calls, /^installer removal-check normal preserve:/m);
        assert.doesNotMatch(calls, /installer delete-data/);
      }
      if (scenario.failRemove) {
        assert.match(readFileSync(join(location, 'diagnostic'), 'utf8'), /removal-file-failed: synthetic/);
        const retry = uninstall();
        assert.equal(retry.status, 0, `${scenario.name}: retry ${readFileSync(join(location, 'calls'), 'utf8')}`);
        assert.equal(existsSync(join(location, 'risunest-sync-manager.exe')), false, `${scenario.name}: retry removes the manager`);
        const retried = readFileSync(join(location, 'calls'), 'utf8').slice(calls.length);
        assert.match(retried, /^installer delete-data:/m);
        assert.match(retried, /^installer remove-files normal:/m);
      }
    }
  } finally {
    assert.equal(dirname(directory), resolve(tmpdir()));
    rmSync(directory, { recursive: true, force: true, maxRetries: 10, retryDelay: 100 });
  }
});

test('Sync preinstall checks the resolved installation directory', { skip }, () => {
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
LoadLanguageFile "${'$'}{NSISDIR}\Contrib\Language files\English.nlf"
LoadLanguageFile "${'$'}{NSISDIR}\Contrib\Language files\Korean.nlf"
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

const setupScript = (directory, hook, body) => String.raw`
Unicode true
RequestExecutionLevel user
SilentInstall silent
OutFile "${q(join(directory, 'installer.exe'))}"
!include LogicLib.nsh
!include FileFunc.nsh
LoadLanguageFile "${'$'}{NSISDIR}\Contrib\Language files\English.nlf"
LoadLanguageFile "${'$'}{NSISDIR}\Contrib\Language files\Korean.nlf"
!define MAINBINARYNAME "SyntheticSync"
!define PRODUCTNAME "Synthetic Sync"
!macro CheckIfAppIsRunning executable product
!macroend
!include "${q(hook)}"
Section
${body}
SectionEnd
`;

test('Sync setup keeps the replaced bundle list until a retry removes obsolete files', { skip }, () => {
  const directory = mkdtempSync(join(tmpdir(), 'risunest-sync-nsis-baseline-'));
  try {
    const binary = buildManagerStub(directory);
    const replacement = join(directory, 'replacement.json');
    writeFileSync(replacement, '{"version":"1.0.5","files":["risunest-sync-manager.exe"]}\n');
    const script = join(directory, 'installer.nsi');
    writeFileSync(script, setupScript(directory, writeManagerHook(directory), String.raw`
!insertmacro NSIS_HOOK_PREINSTALL
SetOutPath "$INSTDIR"
File /oname=risunest-sync-manager.exe "${q(binary)}"
File /oname=risunest-sync-bundle.json "${q(replacement)}"
!insertmacro NSIS_HOOK_POSTINSTALL`));
    const compiled = spawnSync(compiler, ['/V2', script], { encoding: 'utf8', timeout: 30000, windowsHide: true });
    assert.equal(compiled.status, 0, `${compiled.stdout}\n${compiled.stderr}`);
    const install = location => spawnSync(join(directory, 'installer.exe'), ['/S', `/D=${location}`], { timeout: 30000, windowsHide: true });
    const location = join(directory, 'installed');
    const baseline = join(location, 'risunest-sync-bundle.previous.json');
    const previous = '{"version":"1.0.4","files":["risunest-sync-manager.exe","risunest-sync-retired.exe"]}\n';
    mkdirSync(location);
    writeFileSync(join(location, 'risunest-sync-bundle.json'), previous);
    copyFileSync(binary, join(location, 'risunest-sync-manager.exe'));
    writeFileSync(join(location, 'fail-register'), 'fixture');
    assert.equal(install(location).status, 1);
    assert.equal(readFileSync(join(location, 'risunest-sync-bundle.json'), 'utf8'), readFileSync(replacement, 'utf8'));
    assert.equal(readFileSync(baseline, 'utf8'), previous, 'a failed setup keeps the replaced bundle list');
    assert.equal(install(location).status, 0);
    assert.equal(readFileSync(join(location, 'baseline-received'), 'utf8'), previous, 'the retry removes files the first attempt replaced');
    assert.equal(existsSync(baseline), false);
    const fresh = join(directory, 'fresh');
    assert.equal(install(fresh).status, 0);
    assert.equal(existsSync(join(fresh, 'risunest-sync-bundle.previous.json')), false);
    assert.doesNotMatch(readFileSync(join(fresh, 'calls'), 'utf8'), /obsolete-files/);
  } finally {
    assert.equal(dirname(directory), resolve(tmpdir()));
    rmSync(directory, { recursive: true, force: true, maxRetries: 10, retryDelay: 100 });
  }
});

test('Sync setup compares against the version in the installed bundle marker', { skip }, () => {
  const directory = mkdtempSync(join(tmpdir(), 'risunest-sync-nsis-version-'));
  try {
    const script = join(directory, 'installer.nsi');
    writeFileSync(script, setupScript(directory, writeManagerHook(directory), String.raw`
StrCpy $0 "registry"
!insertmacro NSIS_HOOK_INSTALLED_VERSION
FileOpen $1 "$INSTDIR\installed-version" w
FileWrite $1 $0
FileClose $1`));
    const compiled = spawnSync(compiler, ['/V2', script], { encoding: 'utf8', timeout: 30000, windowsHide: true });
    assert.equal(compiled.status, 0, `${compiled.stdout}\n${compiled.stderr}`);
    const generated = {
      schema: 'risunest-sync-bundle/v1', product: 'sync', variant: 'managed', version: '1.0.7', protocolId: 'synthetic', storeFormatId: 'synthetic',
      files: ['risunest-sync-manager.exe'], vendor: [{ name: 'cloudflared', version: '2025.8.1', path: 'cloudflared.exe' }],
    };
    for (const [name, marker, expected] of [
      ['generated', `${JSON.stringify(generated, null, 2)}\n`, '1.0.7'],
      ['compact', JSON.stringify(generated), '1.0.7'],
      ['spaced', '{"schema": "version", "version" :\t"1.0.8"}', '1.0.8'],
      ['missing', undefined, 'registry'],
      ['unversioned', `${JSON.stringify({ ...generated, version: undefined }, null, 2)}\n`, 'registry'],
      ['numeric', '{"version": 107}', 'registry'],
      ['empty', '{"version": ""}', 'registry'],
    ]) {
      const location = join(directory, name);
      mkdirSync(location);
      if (marker !== undefined) writeFileSync(join(location, 'risunest-sync-bundle.json'), marker);
      assert.equal(spawnSync(join(directory, 'installer.exe'), ['/S', `/D=${location}`], { timeout: 30000, windowsHide: true }).status, 0);
      assert.equal(readFileSync(join(location, 'installed-version'), 'utf8'), expected, name);
    }
  } finally {
    assert.equal(dirname(directory), resolve(tmpdir()));
    rmSync(directory, { recursive: true, force: true, maxRetries: 10, retryDelay: 100 });
  }
});
