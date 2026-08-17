[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)]
    [string]$DestinationRoot,

    [string]$SourceRoot = (Resolve-Path (Join-Path $PSScriptRoot "../..")).Path
)

$ErrorActionPreference = "Stop"

function Get-NormalizedFullPath {
    param([string]$Path)
    return [System.IO.Path]::GetFullPath($Path).TrimEnd([System.IO.Path]::DirectorySeparatorChar, [System.IO.Path]::AltDirectorySeparatorChar)
}

function Get-RelativeRepositoryPath {
    param(
        [string]$Root,
        [string]$Path
    )
    return ([System.IO.Path]::GetRelativePath($Root, $Path) -replace "\\", "/")
}

function Copy-DirectoryTree {
    param(
        [string]$Source,
        [string]$Destination
    )
    if (-not (Test-Path -LiteralPath $Source -PathType Container)) {
        return
    }
    New-Item -ItemType Directory -Force -Path $Destination | Out-Null
    Get-ChildItem -LiteralPath $Source -Recurse -File | ForEach-Object {
        $relative = [System.IO.Path]::GetRelativePath($Source, $_.FullName)
        $target = Join-Path $Destination $relative
        New-Item -ItemType Directory -Force -Path (Split-Path -Parent $target) | Out-Null
        Copy-Item -LiteralPath $_.FullName -Destination $target
    }
}

$resolvedSource = (Resolve-Path $SourceRoot).Path
$source = Get-NormalizedFullPath $resolvedSource
$destination = Get-NormalizedFullPath $DestinationRoot
$comparison = if ($env:OS -eq "Windows_NT") { [StringComparison]::OrdinalIgnoreCase } else { [StringComparison]::Ordinal }
$separator = [System.IO.Path]::DirectorySeparatorChar

if ($destination.Equals($source, $comparison) -or $destination.StartsWith("$source$separator", $comparison)) {
    throw "DestinationRoot must be outside SourceRoot."
}
if ($source.StartsWith("$destination$separator", $comparison)) {
    throw "DestinationRoot may not contain SourceRoot."
}

if (Test-Path -LiteralPath $destination) {
    if (@(Get-ChildItem -LiteralPath $destination -Force).Count -ne 0) {
        throw "DestinationRoot must be absent or empty: $destination"
    }
} else {
    New-Item -ItemType Directory -Force -Path $destination | Out-Null
}

$requiredRootFiles = @(
    "Cargo.toml",
    "Cargo.lock",
    "rust-toolchain.toml",
    "rustfmt.toml",
    ".gitattributes",
    ".gitignore",
    "AGENTS.md",
    "CLAUDE.md",
    "README.md"
)
foreach ($relative in $requiredRootFiles) {
    $from = Join-Path $source $relative
    if (-not (Test-Path -LiteralPath $from -PathType Leaf)) {
        throw "Required public source file is missing: $relative"
    }
    Copy-Item -LiteralPath $from -Destination (Join-Path $destination $relative)
}

foreach ($directory in @("crates", "examples", "scripts")) {
    Copy-DirectoryTree `
        -Source (Join-Path $source $directory) `
        -Destination (Join-Path $destination $directory)
}

$sourceDocs = Join-Path $source "docs"
$destinationDocs = Join-Path $destination "docs"
New-Item -ItemType Directory -Force -Path $destinationDocs | Out-Null
Get-ChildItem -LiteralPath $sourceDocs -Recurse -File -Filter "*.md" | ForEach-Object {
    $relative = [System.IO.Path]::GetRelativePath($sourceDocs, $_.FullName)
    $target = Join-Path $destinationDocs $relative
    New-Item -ItemType Directory -Force -Path (Split-Path -Parent $target) | Out-Null
    Copy-Item -LiteralPath $_.FullName -Destination $target
}

$sourceWorkflowRoot = Join-Path $source ".github/workflows"
$destinationWorkflowRoot = Join-Path $destination ".github/workflows"
$publicWorkflows = @(
    "gameengine-chatgpt-dispatch-trigger.yml",
    "gameengine-chatgpt-dispatcher.yml",
    "gameengine-windows-validation.yml"
)
New-Item -ItemType Directory -Force -Path $destinationWorkflowRoot | Out-Null
foreach ($workflow in $publicWorkflows) {
    $from = Join-Path $sourceWorkflowRoot $workflow
    if (-not (Test-Path -LiteralPath $from -PathType Leaf)) {
        throw "Required public workflow template is missing: $workflow"
    }
    Copy-Item -LiteralPath $from -Destination (Join-Path $destinationWorkflowRoot $workflow)
}

$allowlistPath = Join-Path $source "scripts/public/public-asset-allowlist.txt"
$assetAllowlist = @{}
Get-Content -LiteralPath $allowlistPath -Encoding utf8 | ForEach-Object {
    $line = $_.Trim()
    if ($line -and -not $line.StartsWith("#")) {
        $assetAllowlist[$line -replace "\\", "/"] = $true
    }
}

$reviewExtensions = @(
    ".png", ".jpg", ".jpeg", ".gif", ".webp", ".bmp", ".tga",
    ".fbx", ".gltf", ".glb", ".pmx", ".pmd", ".vmd", ".obj",
    ".wav", ".mp3", ".ogg", ".flac",
    ".ttf", ".otf", ".woff", ".woff2",
    ".pptx", ".docx", ".zip", ".7z", ".rar"
)
$unreviewedAssets = @()
Get-ChildItem -LiteralPath $destination -Recurse -File | ForEach-Object {
    if ($reviewExtensions -contains $_.Extension.ToLowerInvariant()) {
        $relative = Get-RelativeRepositoryPath $destination $_.FullName
        if (-not $assetAllowlist.ContainsKey($relative)) {
            $unreviewedAssets += $relative
        }
    }
}
if ($unreviewedAssets.Count -gt 0) {
    throw "Public snapshot contains unreviewed asset files: $($unreviewedAssets -join ', ')"
}

$forbiddenTopLevel = @(
    ".codex_tmp",
    ".idea",
    ".vscode",
    "GameEngine-ChatGPT-Apply",
    "build_errors.txt",
    "crates.zip",
    "docs.zip",
    "outputs"
)
foreach ($relative in $forbiddenTopLevel) {
    if (Test-Path -LiteralPath (Join-Path $destination $relative)) {
        throw "Forbidden private/generated path was exported: $relative"
    }
}

$textExtensions = @(
    ".rs", ".toml", ".md", ".yml", ".yaml", ".json", ".ps1", ".sh",
    ".wgsl", ".txt", ".rhai", ".gitignore", ".gitattributes"
)
$detectors = [ordered]@{
    "private-key-header" = '-----BEGIN (?:RSA |EC |OPENSSH |DSA )?PRIVATE KEY-----'
    "github-token" = '(?<![A-Za-z0-9_])(?:gh[pousr]_[A-Za-z0-9]{20,}|github_pat_[A-Za-z0-9_]{20,})(?![A-Za-z0-9_])'
    "aws-access-key" = '(?<![A-Z0-9])(?:AKIA|ASIA)[A-Z0-9]{16}(?![A-Z0-9])'
    "private-user-path" = '(?i)\b[A-Z]:\\Users\\(?!\.\.\.\\)[^\\\s]+\\'
}
$findings = @()
Get-ChildItem -LiteralPath $destination -Recurse -File | ForEach-Object {
    $extension = $_.Extension.ToLowerInvariant()
    if (-not $textExtensions.Contains($extension) -and $_.Name -notin @(".gitignore", ".gitattributes")) {
        return
    }
    $text = Get-Content -Raw -LiteralPath $_.FullName -Encoding utf8
    foreach ($detector in $detectors.GetEnumerator()) {
        if ($text -match $detector.Value) {
            $findings += "$(Get-RelativeRepositoryPath $destination $_.FullName):$($detector.Key)"
        }
    }
}
if ($findings.Count -gt 0) {
    throw "Public snapshot security scan found high-confidence findings (values suppressed): $($findings -join ', ')"
}

$manifest = @(
    Get-ChildItem -LiteralPath $destination -Recurse -File |
        ForEach-Object { Get-RelativeRepositoryPath $destination $_.FullName } |
        Sort-Object
)
$manifestPath = Join-Path $destination ".public-snapshot-manifest.txt"
$manifest | Set-Content -LiteralPath $manifestPath -Encoding utf8

Write-Host "Public snapshot exported to $destination"
Write-Host "Files: $($manifest.Count)"
Write-Host "Reviewed asset files: $($assetAllowlist.Count)"
