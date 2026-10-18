[CmdletBinding()]
param(
    [string] $SdkRoot,
    [string] $NdkRoot,
    [string] $CheckerPath = (Join-Path $PSScriptRoot '../util/checkAndroidAlignment.ps1')
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

function New-ElfFixture([UInt64] $Alignment) {
    $stream = [IO.MemoryStream]::new()
    $writer = [IO.BinaryWriter]::new($stream)
    try {
        $writer.Write([byte[]] @(0x7f, 0x45, 0x4c, 0x46, 2, 1, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0))
        $writer.Write([UInt16] 3)
        $writer.Write([UInt16] 183)
        $writer.Write([UInt32] 1)
        $writer.Write([UInt64] 0)
        $writer.Write([UInt64] 64)
        $writer.Write([UInt64] 0)
        $writer.Write([UInt32] 0)
        foreach ($value in @(64, 56, 1, 64, 0, 0)) { $writer.Write([UInt16] $value) }
        $writer.Write([UInt32] 1)
        $writer.Write([UInt32] 5)
        foreach ($value in @(0, 0, 0, 120, 120)) { $writer.Write([UInt64] $value) }
        $writer.Write($Alignment)
        return ,$stream.ToArray()
    }
    finally { $writer.Dispose(); $stream.Dispose() }
}

function New-ApkFixture([string] $Path, [object[]] $Entries) {
    $archive = [IO.Compression.ZipFile]::Open($Path, [IO.Compression.ZipArchiveMode]::Create)
    try {
        foreach ($item in $Entries) {
            $entry = $archive.CreateEntry($item.Name, [IO.Compression.CompressionLevel]::Optimal)
            $stream = $entry.Open()
            try { $stream.Write($item.Bytes, 0, $item.Bytes.Length) }
            finally { $stream.Dispose() }
        }
    }
    finally { $archive.Dispose() }
}

$tempRoot = [IO.Path]::GetFullPath([IO.Path]::GetTempPath())
$tempPrefix = 'risu-android-alignment-test-'
$tempDirectory = Join-Path $tempRoot ($tempPrefix + [Guid]::NewGuid().ToString('N'))
[void] [IO.Directory]::CreateDirectory($tempDirectory)
try {
    $aligned = New-ElfFixture 16384
    $cases = @(
        @{
            Name = 'case-sensitive-resources'
            Entries = @(
                @{ Name = 'res/2F.xml'; Bytes = [byte[]] @(1) },
                @{ Name = 'res/2f.xml'; Bytes = [byte[]] @(2) },
                @{ Name = 'lib/arm64-v8a/libfixture.so'; Bytes = $aligned }
            )
            Failure = $null
        },
        @{
            Name = 'missing-native-library'
            Entries = @(@{ Name = 'res/fixture.xml'; Bytes = [byte[]] @(1) })
            Failure = 'APK contains no lib/arm64-v8a directory'
        },
        @{
            Name = 'duplicate-native-library'
            Entries = @(
                @{ Name = 'lib/arm64-v8a/libfixture.so'; Bytes = $aligned },
                @{ Name = 'lib/arm64-v8a/libfixture.so'; Bytes = $aligned }
            )
            Failure = 'already exists'
        },
        @{
            Name = 'misaligned-native-library'
            Entries = @(@{ Name = 'lib/arm64-v8a/libfixture.so'; Bytes = (New-ElfFixture 4096) })
            Failure = 'LOAD alignment below 2\*\*14'
        }
    )
    foreach ($case in $cases) {
        $apkPath = Join-Path $tempDirectory ($case.Name + '.apk')
        New-ApkFixture $apkPath $case.Entries
        $failure = $null
        $output = @()
        try { $output = @(& $CheckerPath -ApkPath $apkPath -SdkRoot $SdkRoot -NdkRoot $NdkRoot) }
        catch { $failure = $_ }
        if ($case.Failure) {
            if (-not $failure -or $failure.ToString() -notmatch $case.Failure) {
                throw "$($case.Name): expected $($case.Failure), received $failure"
            }
        }
        else {
            if ($failure) { throw $failure }
            if ($output -notcontains 'Checked arm64 libraries: 1') { throw 'Aligned library was not checked' }
        }
        Write-Output "PASS $($case.Name)"
    }
}
finally {
    $fullPath = [IO.Path]::GetFullPath($tempDirectory)
    if ([IO.Directory]::GetParent($fullPath).FullName -cne $tempRoot.TrimEnd([IO.Path]::DirectorySeparatorChar) -or
        -not [IO.Path]::GetFileName($fullPath).StartsWith($tempPrefix, [StringComparison]::Ordinal)) {
        throw "Refusing to remove unexpected fixture path: $fullPath"
    }
    [IO.Directory]::Delete($fullPath, $true)
}
