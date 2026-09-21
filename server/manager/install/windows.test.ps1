$ErrorActionPreference = "Stop"
$tempRoot = [IO.Path]::GetFullPath([IO.Path]::GetTempPath())
$testRoot = Join-Path $tempRoot ("risunest-nsis-guard-" + [guid]::NewGuid().ToString("N"))
New-Item -ItemType Directory -Path $testRoot | Out-Null

try {
    $fakeSource = @'
use std::{env, fs::OpenOptions, io::Write};

fn main() {
    let args = env::args().skip(1).collect::<Vec<_>>();
    if let Ok(log) = env::var("RISUNEST_NSIS_TEST_LOG") {
        let mut file = OpenOptions::new().create(true).append(true).open(log).unwrap();
        writeln!(file, "{}", args.join("|")).unwrap();
    }
    if args.as_slice() == ["installer", "prepare"] {
        println!("{}", "a".repeat(64));
        return;
    }
    if args.len() == 3 && args[0] == "installer" && args[1] == "finish" {
        std::process::exit(if args[2] == "a".repeat(64) { 0 } else { 41 });
    }
    if args.first().map(String::as_str) == Some("autostart")
        && env::var("RISUNEST_NSIS_FAIL_AUTOSTART").as_deref() == Ok("1")
    {
        std::process::exit(42);
    }
}
'@
    $fakeSourcePath = Join-Path $testRoot "fake-manager.rs"
    $fakeSource | Set-Content -Encoding utf8 $fakeSourcePath
    $fake = Join-Path $testRoot "risunest-sync-manager.exe"
    & rustc --edition=2021 $fakeSourcePath -o $fake
    if ($LASTEXITCODE -ne 0) {
        throw "rustc failed with exit code $LASTEXITCODE."
    }

    $installer = Join-Path $testRoot "guard-test.exe"
    $hook = (Resolve-Path (Join-Path $PSScriptRoot "windows.nsh")).Path
    $defaultInstallDir = Join-Path $testRoot "default-install"
    $installDir = Join-Path $testRoot "installed"
    $payload = Join-Path $testRoot "hook-output.txt"
    "installed" | Set-Content -Encoding utf8 $payload
    $nsi = Join-Path $testRoot "guard-test.nsi"
    @"
Unicode true
Name "RisuNest guard test"
OutFile "$installer"
InstallDir "`$TEMP\RisuNestGuardTest"
RequestExecutionLevel user
SilentInstall silent
!include LogicLib.nsh
!include FileFunc.nsh
Var DeleteAppDataCheckboxState
Var UpdateMode
!define RISUNEST_SYNC_DEFAULT_INSTALL_DIR "$defaultInstallDir"
!define RISUNEST_SYNC_INSTALL_DIR "$installDir"
!macro CheckIfAppIsRunning APP PRODUCT
!macroend
!include "$hook"
Section
  SetOutPath `$INSTDIR
  !insertmacro NSIS_HOOK_PREINSTALL
  File /oname=hook-output.txt "$payload"
  CreateShortcut "`$INSTDIR\RisuNest Sync.lnk" "`$INSTDIR\hook-output.txt"
  !insertmacro NSIS_HOOK_POSTINSTALL
SectionEnd
Section "Uninstall"
  !insertmacro NSIS_HOOK_PREUNINSTALL
  !insertmacro NSIS_HOOK_POSTUNINSTALL
SectionEnd
"@ | Set-Content -Encoding utf8 $nsi

    $makeNsis = Join-Path $env:LOCALAPPDATA "tauri/NSIS/makensis.exe"
    if (!(Test-Path $makeNsis)) {
        throw "Tauri NSIS toolchain is unavailable."
    }
    & $makeNsis /V2 $nsi
    if ($LASTEXITCODE -ne 0) {
        throw "makensis failed with exit code $LASTEXITCODE."
    }

    New-Item -ItemType Directory -Path $installDir | Out-Null
    Copy-Item $fake (Join-Path $installDir "risunest-sync-manager.exe")
    $log = Join-Path $testRoot "commands.log"
    $env:RISUNEST_NSIS_TEST_LOG = $log
    $process = Start-Process -FilePath $installer -ArgumentList @("/S", "/D=$defaultInstallDir") -Wait -PassThru -WindowStyle Hidden
    if ($process.ExitCode -ne 0) {
        throw "NSIS success harness exited with $($process.ExitCode)."
    }
    if (!(Test-Path (Join-Path $installDir "hook-output.txt"))) {
        throw "NSIS copied files to the output directory selected before the install hook changed `$INSTDIR."
    }
    if (Test-Path (Join-Path $defaultInstallDir "hook-output.txt")) {
        throw "NSIS left installed files in the superseded install directory."
    }
    $shortcutPath = Join-Path $installDir "RisuNest Sync.lnk"
    $shortcut = (New-Object -ComObject WScript.Shell).CreateShortcut($shortcutPath)
    if ($shortcut.TargetPath -ne (Join-Path $installDir "hook-output.txt") -or !(Test-Path -LiteralPath $shortcut.TargetPath)) {
        throw "NSIS created a shortcut whose target does not match the installed file location."
    }
    $expected = "installer|finish|" + ("a" * 64)
    if ((Get-Content $log) -notcontains $expected) {
        throw "NSIS did not pass the exact 64-character nonce to installer finish."
    }

    $env:RISUNEST_NSIS_FAIL_AUTOSTART = "1"
    $process = Start-Process -FilePath $installer -ArgumentList @("/S", "/D=$defaultInstallDir") -Wait -PassThru -WindowStyle Hidden
    if ($process.ExitCode -eq 0) {
        throw "A quiet postinstall failure returned success."
    }
    Write-Output "NSIS runtime guard tests passed."
}
finally {
    Remove-Item Env:RISUNEST_NSIS_TEST_LOG -ErrorAction SilentlyContinue
    Remove-Item Env:RISUNEST_NSIS_FAIL_AUTOSTART -ErrorAction SilentlyContinue
    $resolved = [IO.Path]::GetFullPath($testRoot)
    if (!$resolved.StartsWith($tempRoot, [StringComparison]::OrdinalIgnoreCase)) {
        throw "Refusing to remove a test directory outside the system temp directory."
    }
    Remove-Item -LiteralPath $resolved -Recurse -Force -ErrorAction SilentlyContinue
}
