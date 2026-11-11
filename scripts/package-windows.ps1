[CmdletBinding()]
param(
    [switch]$PrepareOnly,
    [string]$PlatformToolsUrl = 'https://dl.google.com/android/repository/platform-tools-latest-windows.zip',
    [string]$Sha256
)

$ErrorActionPreference = 'Stop'
$root = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot '..'))
$vendor = Join-Path $root 'vendor\windows'
$tools = Join-Path $vendor 'platform-tools'
$required = @('adb.exe', 'AdbWinApi.dll', 'AdbWinUsbApi.dll', 'NOTICE.txt', 'source.properties')

if ($Sha256 -and (Test-Path -LiteralPath (Join-Path $tools 'adb.exe'))) {
    throw 'For SHA256 verification use a fresh vendor/windows/platform-tools directory; cached files are not reverified.'
}
if (-not (Test-Path -LiteralPath (Join-Path $tools 'adb.exe'))) {
    $staging = Join-Path $vendor ('download-' + [guid]::NewGuid().ToString('N'))
    New-Item -ItemType Directory -Path $staging -Force | Out-Null
    $archive = Join-Path $staging 'platform-tools.zip'
    Write-Host 'Downloading official Android platform-tools...'
    Invoke-WebRequest -Uri $PlatformToolsUrl -OutFile $archive
    $hash = (Get-FileHash -LiteralPath $archive -Algorithm SHA256).Hash
    if ($Sha256 -and $hash -ne $Sha256) { throw 'Platform-tools SHA256 mismatch.' }
    Expand-Archive -LiteralPath $archive -DestinationPath $staging
    foreach ($name in $required) {
        if (-not (Test-Path -LiteralPath (Join-Path $staging "platform-tools\$name"))) {
            throw "Invalid platform-tools archive: missing $name"
        }
    }
    Copy-Item -LiteralPath (Join-Path $staging 'platform-tools') -Destination $vendor -Recurse -Force
    Set-Content -LiteralPath (Join-Path $vendor 'platform-tools.sha256') -Value $hash -Encoding ascii
}

foreach ($name in $required) {
    if (-not (Test-Path -LiteralPath (Join-Path $tools $name))) { throw "Missing bundled ADB file: $name" }
}
& (Join-Path $tools 'adb.exe') version
if ($LASTEXITCODE -ne 0) { throw 'Bundled ADB cannot run.' }
$scrcpy = Join-Path $vendor 'scrcpy'
$scrcpyVersion = '5.0.1'
$scrcpyHash = 'b12a2c4ee8be317422451fc7dcf8ee20a71b5ea7ef9ad73ddd825a25316227a5'
if (-not (Test-Path -LiteralPath (Join-Path $scrcpy 'scrcpy.exe'))) {
    $staging = Join-Path $vendor ('scrcpy-' + [guid]::NewGuid().ToString('N'))
    New-Item -ItemType Directory -Path $staging -Force | Out-Null
    $archive = Join-Path $staging 'scrcpy.zip'
    Invoke-WebRequest -Uri "https://github.com/Genymobile/scrcpy/releases/download/v$scrcpyVersion/scrcpy-win64-v$scrcpyVersion.zip" -OutFile $archive
    if ((Get-FileHash -LiteralPath $archive -Algorithm SHA256).Hash -ne $scrcpyHash) { throw 'scrcpy SHA256 mismatch.' }
    Expand-Archive -LiteralPath $archive -DestinationPath $staging
    Copy-Item -LiteralPath (Join-Path $staging "scrcpy-win64-v$scrcpyVersion") -Destination $scrcpy -Recurse
}
foreach ($name in @('scrcpy.exe', 'scrcpy-server', 'SDL3.dll', 'avcodec-63.dll', 'avformat-63.dll', 'avutil-61.dll', 'swresample-7.dll', 'LICENSE.txt')) {
    if (-not (Test-Path -LiteralPath (Join-Path $scrcpy $name))) { throw "Missing scrcpy file: $name" }
}
& (Join-Path $scrcpy 'scrcpy.exe') --version
if ($LASTEXITCODE -ne 0) { throw 'Bundled scrcpy cannot run.' }
if ($PrepareOnly) { return }

Push-Location $root
try {
    & cargo build --release --locked
    if ($LASTEXITCODE -ne 0) { throw 'Release build failed. Install Visual C++ Build Tools and use a developer PowerShell.' }
    $metadata = (& cargo metadata --no-deps --format-version 1 --locked | ConvertFrom-Json)
    if ($LASTEXITCODE -ne 0) { throw 'Unable to read Cargo metadata.' }
    $executable = Join-Path $metadata.target_directory 'release\androidtool.exe'
    if (-not (Test-Path -LiteralPath $executable)) { throw "Release executable missing: $executable" }
    $dist = Join-Path $root 'dist'
    $name = 'AndroidTool-windows-' + (Get-Date -Format 'yyyyMMdd-HHmmss') + '-' + [guid]::NewGuid().ToString('N').Substring(0, 8)
    $package = Join-Path $dist $name
    New-Item -ItemType Directory -Path $package -Force | Out-Null
    Copy-Item -LiteralPath $executable -Destination $package
    Copy-Item -LiteralPath $tools -Destination $package -Recurse
    Copy-Item -LiteralPath $scrcpy -Destination $package -Recurse
    Copy-Item -LiteralPath (Join-Path $root 'README.md') -Destination $package
    Copy-Item -LiteralPath (Join-Path $root 'THIRD_PARTY.md') -Destination $package
    Copy-Item -LiteralPath (Join-Path $root 'assets') -Destination $package -Recurse
    if (Test-Path -LiteralPath (Join-Path $vendor 'platform-tools.sha256')) {
        Copy-Item -LiteralPath (Join-Path $vendor 'platform-tools.sha256') -Destination $package
    }
    $zip = "$package.zip"
    Compress-Archive -LiteralPath $package -DestinationPath $zip
    Write-Host "Portable package: $zip"
} finally {
    Pop-Location
}
