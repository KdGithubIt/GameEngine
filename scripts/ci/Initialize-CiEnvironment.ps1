[CmdletBinding()]
param(
    [string]$WorkspaceRoot = (Resolve-Path (Join-Path $PSScriptRoot "../..")).Path,
    [switch]$PersistentRunner
)

$ErrorActionPreference = "Stop"
$toolchainFile = Join-Path $WorkspaceRoot "rust-toolchain.toml"
$toolchainText = Get-Content -Raw -LiteralPath $toolchainFile -Encoding utf8

$channelMatch = [regex]::Match($toolchainText, '(?m)^\s*channel\s*=\s*"([^"]+)"\s*$')
if (-not $channelMatch.Success) {
    throw "rust-toolchain.toml does not define a channel."
}
$channel = $channelMatch.Groups[1].Value

$components = @()
$componentsMatch = [regex]::Match($toolchainText, '(?m)^\s*components\s*=\s*\[(.*)\]\s*$')
if ($componentsMatch.Success) {
    foreach ($component in [regex]::Matches($componentsMatch.Groups[1].Value, '"([^"]+)"')) {
        $components += $component.Groups[1].Value
    }
}

$installed = @(& rustup toolchain list | ForEach-Object { ($_ -split '\s+')[0] })
$hasChannel = $installed | Where-Object { $_ -eq $channel -or $_ -like "$channel-*" } | Select-Object -First 1
if (-not $hasChannel) {
    $arguments = @("toolchain", "install", $channel, "--profile", "minimal")
    foreach ($component in $components) {
        $arguments += @("--component", $component)
    }
    & rustup @arguments
    if ($LASTEXITCODE -ne 0) {
        throw "rustup toolchain install failed with exit code $LASTEXITCODE."
    }
} else {
    foreach ($component in $components) {
        & rustup component add $component --toolchain $channel | Out-Host
        if ($LASTEXITCODE -ne 0) {
            throw "Could not ensure Rust component '$component'."
        }
    }
}

$rustcVersion = & rustc "+$channel" --version
if ($LASTEXITCODE -ne 0 -or -not $rustcVersion.StartsWith("rustc $channel ")) {
    throw "Expected Rust $channel, but got '$rustcVersion'."
}

if ($PersistentRunner) {
    $cacheRoot = $env:GAMEENGINE_CI_CACHE_ROOT
    if (-not $cacheRoot) {
        $cacheRoot = Join-Path $HOME ".gameengine-ci"
    }

    $hostLine = & rustc "+$channel" -vV | Where-Object { $_ -like "host:*" }
    $host = ($hostLine -replace "^host:\s*", "")
    $key = ("$channel-$host" -replace '[^A-Za-z0-9._-]', '_')

    $cargoHome = Join-Path $cacheRoot "cargo"
    $targetDir = Join-Path $cacheRoot "target\$key"
    $sccacheDir = Join-Path $cacheRoot "sccache\$key"
    foreach ($path in @($cargoHome, $targetDir, $sccacheDir)) {
        New-Item -ItemType Directory -Force -Path $path | Out-Null
    }

    if ($env:GITHUB_ENV) {
        Add-Content $env:GITHUB_ENV "CARGO_HOME=$cargoHome"
        Add-Content $env:GITHUB_ENV "CARGO_TARGET_DIR=$targetDir"
        Add-Content $env:GITHUB_ENV "SCCACHE_DIR=$sccacheDir"
        Add-Content $env:GITHUB_ENV "SCCACHE_GHA_ENABLED=false"
    }
    $env:CARGO_HOME = $cargoHome
    $env:CARGO_TARGET_DIR = $targetDir
    $env:SCCACHE_DIR = $sccacheDir
    $env:SCCACHE_GHA_ENABLED = "false"
}
