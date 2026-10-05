import assert from 'node:assert/strict';
import { existsSync, mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { dirname, join, resolve } from 'node:path';
import { spawnSync } from 'node:child_process';
import test from 'node:test';

const compiler = process.env.NSIS_MAKENSIS ?? join(process.env.LOCALAPPDATA ?? '', 'tauri', 'NSIS', 'makensis.exe');
const skip = process.platform !== 'win32' || !existsSync(compiler);
if (process.env.RISUNEST_REQUIRE_NSIS === '1' && skip) throw new Error('NSIS compiler is required');
const quote = value => value.replaceAll('$', '$$').replaceAll('"', '$\\"');
const nonce = 'a'.repeat(64);
const stages = {
  prepare: 'Stopping RisuNest Sync and preparing the installation...',
  schedule: 'Applying scheduled update settings...',
  autostart: 'Registering automatic server startup...',
  start: 'Starting RisuNest Sync and checking the server response...',
  finish: 'Finishing RisuNest Sync installation...',
  check: 'Checking whether RisuNest Sync can be removed...',
  cleanup: 'Stopping the server and removing startup and update tasks...',
  data: 'Removing RisuNest Sync server data...',
  forget: 'Removing RisuNest Sync installation registration...',
  'finish-removal': 'Finishing RisuNest Sync removal...',
  update: 'Stopping RisuNest Sync for the update...',
  app: 'Removing RisuNest application data...',
};

function commandStage(command) {
  if (command.includes('uninstall --dry-run')) return 'check';
  if (command.includes('installer prepare')) return 'prepare';
  if (command.includes('schedule reconcile')) return 'schedule';
  if (command.includes('autostart install')) return 'autostart';
  if (command.includes('start-and-verify')) return 'start';
  if (command.includes('installer finish')) return 'finish';
  if (command.includes('prepare-update')) return 'update';
  if (command.includes('uninstall lock-held')) return 'cleanup';
  if (command.includes('installer delete-data')) return 'data';
  if (command.includes('forget-removal')) return 'forget';
  throw new Error(`Unrecognized synthetic manager command: ${command}`);
}

test('NSIS displays the active operation and does not complete failed Sync setup or removal', { skip }, () => {
  const root = mkdtempSync(join(tmpdir(), 'risunest-nsis-progress-'));
  const compile = (name, source) => {
    const script = join(root, `${name}.nsi`);
    writeFileSync(script, source);
    const result = spawnSync(compiler, ['/V2', script], { encoding: 'utf8', timeout: 30000, windowsHide: true });
    assert.equal(result.status, 0, `${result.error ?? ''}\n${result.stdout}\n${result.stderr}`);
    return join(root, `${name}.exe`);
  };
  try {
    // Only the manager process boundary is simulated. NSIS executes the hooks,
    // renders its real progress control, and handles Abort itself.
    const syncHook = readFileSync(resolve('server/manager/install/windows.nsh'), 'utf8')
      .replaceAll('$LOCALAPPDATA', '$INSTDIR\\local')
      .replace(/nsExec::ExecToStack '([^\r\n]+)'/g, (_, command) => `!insertmacro Manager "${commandStage(command)}"`)
      .replace('StrCpy $R6 300', '!insertmacro Capture "finish-removal"\n  StrCpy $R6 1');
    writeFileSync(join(root, 'sync.nsh'), syncHook);
    const appHook = readFileSync(resolve('src-tauri/windows.nsh'), 'utf8')
      .replace(/(ExecWait [^\r\n]+)/, '$1\n    !insertmacro Capture "app"');
    writeFileSync(join(root, 'app.nsh'), appHook);
    const cleanup = compile('cleanup', `Unicode true\nRequestExecutionLevel user\nSilentInstall silent\nOutFile "${quote(join(root, 'cleanup.exe'))}"\nSection\nSectionEnd\n`);

    const fixtures = {};
    for (const product of ['sync', 'app']) {
      fixtures[product] = compile(product, String.raw`
Unicode true
RequestExecutionLevel user
AutoCloseWindow true
OutFile "${quote(join(root, `${product}.exe`))}"
!include LogicLib.nsh
!include FileFunc.nsh
!define MAINBINARYNAME "SyntheticRisuNest"
!define PRODUCTNAME "Synthetic RisuNest"
Var UpdateMode
Var DeleteAppDataCheckboxState
Var FixtureFail
Var ProbeWindow
Var ProbeText
Var ProbeFile
Var ProbeHadError
!macro CheckIfAppIsRunning executable product
!macroend
!macro Capture stage
  StrCpy $ProbeHadError 0
  ${'$'}{If} ${'$'}{Errors}
    StrCpy $ProbeHadError 1
  ${'$'}{EndIf}
  FindWindow $ProbeWindow "#32770" "" $HWNDPARENT
  GetDlgItem $ProbeWindow $ProbeWindow 1006
  Push $0
  System::Call 'user32::GetWindowTextW(p $ProbeWindow, w .r0, i ${'$'}{NSIS_MAX_STRLEN})'
  StrCpy $ProbeText $0
  Pop $0
  FileOpen $ProbeFile "$INSTDIR\progress" a
  FileSeek $ProbeFile 0 END
  FileWrite $ProbeFile "${'$'}{stage}|$ProbeText$\r$\n"
  FileClose $ProbeFile
  ${'$'}{If} $ProbeHadError = 1
    SetErrors
  ${'$'}{Else}
    ClearErrors
  ${'$'}{EndIf}
!macroend
!macro Manager stage
  !insertmacro Capture "${'$'}{stage}"
  Push "${nonce}"
  ${'$'}{If} $FixtureFail == "${'$'}{stage}"
    Push 9
  ${'$'}{Else}
    Push 0
  ${'$'}{EndIf}
!macroend
!include "${quote(join(root, `${product}.nsh`))}"
Page instfiles "" HideFixture
UninstPage instfiles "" un.HideFixture
Function HideFixture
  HideWindow
FunctionEnd
Function un.HideFixture
  HideWindow
FunctionEnd
!macro InitFixture
  ReadEnvStr $FixtureFail RISUNEST_NSIS_FIXTURE_FAIL
  ReadEnvStr $UpdateMode RISUNEST_NSIS_FIXTURE_UPDATE
  ReadEnvStr $DeleteAppDataCheckboxState RISUNEST_NSIS_FIXTURE_DELETE
!macroend
Function .onInit
  !insertmacro InitFixture
FunctionEnd
Function un.onInit
  !insertmacro InitFixture
FunctionEnd
!macro Failed
  FileOpen $ProbeFile "$INSTDIR\failed" w
  ${product === 'sync' ? 'FileWrite $ProbeFile "$SyncFailureMessage"' : 'FileWrite $ProbeFile "failed"'}
  FileClose $ProbeFile
!macroend
Function .onInstFailed
  !insertmacro Failed
FunctionEnd
Function un.onUninstFailed
  !insertmacro Failed
FunctionEnd
Section
  SetOutPath "$INSTDIR"
  File /oname=SyntheticRisuNest.exe "${quote(cleanup)}"
  WriteUninstaller "$INSTDIR\uninstall.exe"
  ${product === 'sync' ? '!insertmacro NSIS_HOOK_PREINSTALL\n  DetailPrint "Create shortcut: synthetic.lnk"\n  !insertmacro NSIS_HOOK_POSTINSTALL' : ''}
  FileOpen $ProbeFile "$INSTDIR\installed" w
  FileClose $ProbeFile
SectionEnd
Section Uninstall
  SetAutoClose true
  !insertmacro NSIS_HOOK_PREUNINSTALL
  DetailPrint "Delete: synthetic program files"
  ${product === 'sync' ? '!insertmacro NSIS_HOOK_POSTUNINSTALL' : ''}
  FileOpen $ProbeFile "$INSTDIR\removed" w
  FileClose $ProbeFile
SectionEnd
`);
    }

    const run = (product, name, { remove = false, deleteData = false, update = false, existing = true, fail = '' } = {}) => {
      const location = join(root, name);
      const data = join(location, 'local/RisuNestSyncData');
      mkdirSync(data, { recursive: true });
      writeFileSync(join(data, 'risunest-sync-instance.json'), '{}');
      if (existing) writeFileSync(join(location, 'risunest-sync-manager.exe'), 'synthetic manager boundary');
      const env = { ...process.env, RISUNEST_SYNC_UNINSTALL_DATA_DIR: '', RISUNEST_NSIS_FIXTURE_FAIL: remove ? '' : fail, RISUNEST_NSIS_FIXTURE_DELETE: deleteData ? '1' : '0', RISUNEST_NSIS_FIXTURE_UPDATE: update ? '1' : '0' };
      const install = spawnSync(fixtures[product], [...(!remove && fail ? ['/S'] : []), `/D=${location}`], { env, timeout: 30000, windowsHide: true });
      assert.equal(install.status, !remove && fail ? 1 : 0, `${name}: install ${install.error ?? ''}`);
      if (remove) {
        rmSync(join(location, 'progress'), { force: true });
        if (fail === 'wait') {
          mkdirSync(join(data, 'manager-update'), { recursive: true });
          writeFileSync(join(data, `manager-update/installer-${nonce}.ready`), '{}');
        }
        const result = spawnSync(join(location, 'uninstall.exe'), [...(fail ? ['/S'] : []), `_?=${location}`], { env: { ...env, RISUNEST_NSIS_FIXTURE_FAIL: fail }, timeout: 30000, windowsHide: true });
        assert.equal(result.status, fail ? 1 : 0, `${name}: removal ${result.error ?? ''}\n${existsSync(join(location, 'progress')) ? readFileSync(join(location, 'progress'), 'utf8') : 'no progress'}`);
      }
      assert.equal(existsSync(join(location, remove ? 'removed' : 'installed')), !fail, `${name}: completion`);
      assert.equal(existsSync(join(location, 'failed')), !!fail, `${name}: failure callback`);
      const progress = existsSync(join(location, 'progress')) ? readFileSync(join(location, 'progress'), 'utf8').trim().split(/\r?\n/).map(line => line.split('|')) : [];
      return { progress, failure: fail ? readFileSync(join(location, 'failed'), 'utf8') : '' };
    };
    const expectStages = (result, names, overrides = {}) => {
      assert.deepEqual(result.progress, names.map(name => [name, overrides[name] ?? stages[name]]));
    };
    expectStages(run('sync', 'install'), ['prepare', 'schedule', 'autostart', 'start', 'finish']);
    expectStages(run('sync', 'fresh', { existing: false }), ['schedule', 'autostart', 'start']);
    const removing = { prepare: 'Stopping RisuNest Sync and preparing removal...', finish: 'Preparing to remove RisuNest Sync server data...' };
    expectStages(run('sync', 'preserve', { remove: true }), ['check', 'prepare', 'cleanup', 'finish-removal', 'forget'], removing);
    expectStages(run('sync', 'delete', { remove: true, deleteData: true }), ['check', 'prepare', 'cleanup', 'finish', 'data', 'forget'], removing);
    expectStages(run('sync', 'update', { remove: true, update: true, deleteData: true }), ['update']);
    expectStages(run('app', 'app-delete', { remove: true, deleteData: true }), ['app']);
    expectStages(run('app', 'app-preserve', { remove: true }), []);
    expectStages(run('app', 'app-update', { remove: true, update: true, deleteData: true }), []);

    for (const [stage, message] of [
      ['prepare', /prepared for installation/], ['schedule', /Scheduled update settings could not be applied/],
      ['autostart', /Automatic server startup could not be registered/], ['start', /could not be started or verified/],
      ['finish', /installation could not be finalized/],
    ]) {
      const result = run('sync', `install-fail-${stage}`, { fail: stage });
      assert.match(result.failure, message);
      assert.equal(result.progress.at(-1)[0], stage, 'failed installation stops before the next manager command');
    }
    assert.match(run('sync', 'fresh-failure', { existing: false, fail: 'autostart' }).failure, /Automatic server startup could not be registered/);
    for (const [stage, message] of [
      ['check', /removal checks did not pass/], ['prepare', /prepared for removal/],
      ['cleanup', /could not stop the server or remove/], ['finish', /preparing to remove server data/],
      ['data', /server data could not be completely removed/], ['forget', /installation registration could not be removed/], ['update', /prepared for the update/],
    ]) {
      const result = run('sync', `remove-fail-${stage}`, { remove: true, deleteData: true, update: stage === 'update', fail: stage });
      assert.match(result.failure, message);
      assert.equal(result.progress.at(-1)[0], stage, 'failed removal stops before the next manager command');
    }
    const waiting = run('sync', 'remove-wait-failure', { remove: true, fail: 'wait' });
    assert.match(waiting.failure, /program files were removed, but removal could not be finalized/);
    assert.equal(waiting.progress.at(-1)[0], 'finish-removal');
  } finally {
    assert.equal(dirname(root), resolve(tmpdir()));
    rmSync(root, { recursive: true, force: true });
  }
});
