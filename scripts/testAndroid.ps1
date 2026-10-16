$ErrorActionPreference = 'Stop'
Push-Location (Join-Path $PSScriptRoot '../src-tauri/gen/android')
try {
    $gradle = if ($IsWindows) { './gradlew.bat' } else { './gradlew' }
    # Separate invocations avoid Gradle test-worker shutdown hangs between suites.
    foreach ($task in @(':app:testArmDebugUnitTest', ':app:testLowMemorySafCopy', ':tauri-plugin-barcode-scanner:testDebugUnitTest')) {
        & $gradle $task --no-daemon
        if ($LASTEXITCODE -ne 0) { throw "Android tests failed ($task) with exit code $LASTEXITCODE" }
    }
} finally {
    Pop-Location
}
