# stp installer for Windows (PowerShell).
#
#   irm https://raw.githubusercontent.com/stimma-ai/stimma-tools-protocol-cli/main/install.ps1 | iex
#
# Fetches a prebuilt binary from GitHub Releases (no Rust required). While the repo is private
# it uses the GitHub CLI (`gh`) automatically. Env overrides:
#   $env:STP_VERSION = 'v0.1.0'      # specific tag (default: latest)
#   $env:STP_INSTALL_DIR = '...'     # install location
$ErrorActionPreference = 'Stop'

$Repo = 'stimma-ai/stimma-tools-protocol-cli'
$InstallDir = if ($env:STP_INSTALL_DIR) { $env:STP_INSTALL_DIR } else { "$env:LOCALAPPDATA\Programs\stp" }

function Have($name) { [bool](Get-Command $name -ErrorAction SilentlyContinue) }

function Get-Target {
  $arch = $env:PROCESSOR_ARCHITECTURE
  switch ($arch) {
    'AMD64' { 'x86_64-pc-windows-msvc' }
    'ARM64' { 'aarch64-pc-windows-msvc' }
    default { throw "unsupported architecture: $arch" }
  }
}

function Get-LatestTag {
  if ($env:STP_VERSION) { return $env:STP_VERSION }
  if (Have gh) {
    $t = gh release view --repo $Repo --json tagName -q .tagName 2>$null
    if ($t) { return $t.Trim() }
  }
  $r = Invoke-RestMethod "https://api.github.com/repos/$Repo/releases/latest"
  return $r.tag_name
}

function Get-Asset($tag, $asset, $out) {
  if (Have gh) {
    gh release download $tag --repo $Repo -p $asset -O $out --clobber 2>$null
    if (Test-Path $out) { return }
  }
  $url = "https://github.com/$Repo/releases/download/$tag/$asset"
  Invoke-WebRequest -Uri $url -OutFile $out
}

$target = Get-Target
$tag = Get-LatestTag
if (-not $tag) { throw "could not determine the latest release" }
$asset = "stp-$target.zip"

Write-Host "installing stp $tag for $target…"
$tmp = New-Item -ItemType Directory -Path ([System.IO.Path]::GetTempPath()) -Name "stp-$([guid]::NewGuid())"
$zip = Join-Path $tmp $asset
Get-Asset $tag $asset $zip
Expand-Archive -Path $zip -DestinationPath $tmp -Force

New-Item -ItemType Directory -Force -Path $InstallDir | Out-Null
$bin = Get-ChildItem -Path $tmp -Recurse -Filter stp.exe | Select-Object -First 1
Copy-Item $bin.FullName (Join-Path $InstallDir 'stp.exe') -Force
Remove-Item -Recurse -Force $tmp

Write-Host ""
Write-Host "installed stp -> $InstallDir\stp.exe"
& "$InstallDir\stp.exe" version

$userPath = [Environment]::GetEnvironmentVariable('Path', 'User')
if ($userPath -notlike "*$InstallDir*") {
  [Environment]::SetEnvironmentVariable('Path', "$userPath;$InstallDir", 'User')
  Write-Host ""
  Write-Host "added $InstallDir to your user PATH — restart your shell to pick it up."
}
