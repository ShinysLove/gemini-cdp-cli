param(
    [Parameter(Mandatory = $true)]
    [ValidatePattern('^\d+\.\d+\.\d+$')]
    [string]$Version
)
$ErrorActionPreference = 'Stop'
$repoRoot = Split-Path -Parent $PSScriptRoot
$manifest = Get-Content -LiteralPath (Join-Path $repoRoot 'Cargo.toml') -Raw
if ($manifest -notmatch ('(?m)^version = "' + [regex]::Escape($Version) + '"')) {
    throw 'Package version must match Cargo.toml'
}
$packageName = "EVIL-GEMINI-v$Version-windows-x64"
$outputRoot = Join-Path $repoRoot 'target/release-package'
$packageRoot = Join-Path $outputRoot $packageName
New-Item -ItemType Directory -Path $packageRoot -Force | Out-Null
foreach ($name in @('evil-gemini.exe', 'WebView2Loader.dll')) {
    Copy-Item -LiteralPath (Join-Path $repoRoot "target/release/$name") -Destination (Join-Path $packageRoot $name) -Force
}
foreach ($name in @('README.md', 'config.json')) {
    Copy-Item -LiteralPath (Join-Path $repoRoot $name) -Destination (Join-Path $packageRoot $name) -Force
}
@"
EVIL GEMINI v$Version - Windows x64

1. Extract the entire ZIP to a writable folder.
2. Install Google Chrome and Microsoft Edge WebView2 Runtime if needed.
3. If Chrome is installed elsewhere, edit chrome_binary in config.json.
4. Run evil-gemini.exe. Keep WebView2Loader.dll and config.json next to it.

Chats, attachments, project reports and browser profiles are stored locally
in this folder. No personal data or browser profile is included in this ZIP.
Google CAPTCHA, when requested, must be completed manually in Chrome.

See README.md for the chat, coding agents and approval controls.
"@ | Set-Content -LiteralPath (Join-Path $packageRoot 'START-HERE.txt') -Encoding UTF8
$archive = Join-Path $outputRoot "$packageName.zip"
Compress-Archive -LiteralPath $packageRoot -DestinationPath $archive -Force
$hash = (Get-FileHash -LiteralPath $archive -Algorithm SHA256).Hash.ToLowerInvariant()
"$hash  $packageName.zip" | Set-Content -LiteralPath (Join-Path $outputRoot 'SHA256SUMS.txt') -Encoding ASCII
Get-Item -LiteralPath $archive | Select-Object FullName, Length
Get-Content -LiteralPath (Join-Path $outputRoot 'SHA256SUMS.txt')
