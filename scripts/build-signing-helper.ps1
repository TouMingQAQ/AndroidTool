[CmdletBinding()]
param()
$ErrorActionPreference = 'Stop'
$root = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot '..'))
$assets = Join-Path $root 'assets\signing'
$classes = Join-Path $root 'target\signing-helper-classes'
New-Item -ItemType Directory -Path $assets, $classes -Force | Out-Null
$library = Join-Path $assets 'apksig-8.13.0.jar'
$expected = 'c070ed1394629d74641aa0906f60b2ffa1ee77e6366a1f93437f59717b1aeb89'
if (-not (Test-Path -LiteralPath $library)) {
    Invoke-WebRequest 'https://dl.google.com/dl/android/maven2/com/android/tools/build/apksig/8.13.0/apksig-8.13.0.jar' -OutFile $library
}
if ((Get-FileHash -LiteralPath $library -Algorithm SHA256).Hash.ToLowerInvariant() -ne $expected) {
    throw 'apksig SHA-256 mismatch'
}
& javac --release 17 -encoding UTF-8 -cp $library -d $classes (Join-Path $root 'java\AndroidToolSigner.java')
if ($LASTEXITCODE -ne 0) { throw 'Signing helper compilation failed (requires JDK 17+)' }
& jar --create --file (Join-Path $assets 'androidtool-signing.jar') --date=2026-01-01T00:00:00Z -C $classes AndroidToolSigner.class
if ($LASTEXITCODE -ne 0) { throw 'Signing helper packaging failed' }
