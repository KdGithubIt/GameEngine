[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)]
    [ValidateSet("pull_request", "push", "merge_group", "workflow_dispatch", "schedule")]
    [string]$EventName,

    [Parameter(Mandatory = $true)]
    [string]$ChangedFilesPath,

    [string]$WorkspaceRoot = (Resolve-Path (Join-Path $PSScriptRoot "../..")).Path,
    [string]$RepositoryRoot = "",
    [string]$MetadataFile = "",
    [string]$OutputPath = ""
)

$ErrorActionPreference = "Stop"

function Convert-ToRepositoryPath {
    param([string]$Path)
    $normalized = ($Path -replace "\\", "/").Trim()
    while ($normalized.StartsWith("./")) {
        $normalized = $normalized.Substring(2)
    }
    return $normalized
}

function Join-RepositoryPath {
    param(
        [string]$Prefix,
        [string]$Path
    )
    $child = Convert-ToRepositoryPath $Path
    if (-not $Prefix -or $Prefix -eq ".") {
        return $child
    }
    if (-not $child) {
        return $Prefix
    }
    return "$Prefix/$child"
}

function New-Plan {
    param(
        [string]$Mode,
        [bool]$Skip,
        [string]$Reason,
        [string[]]$ChangedPackages,
        [string[]]$AffectedPackages
    )

    $changed = @($ChangedPackages | Sort-Object -Unique)
    $affected = @($AffectedPackages | Sort-Object -Unique)
    $targets = @()
    if ($Mode -ne "full") {
        $targets = @($affected)
    }

    return [ordered]@{
        schema_version = 1
        validation_mode = $Mode
        skip = $Skip
        reason = $Reason
        changed_packages = $changed
        affected_packages = $affected
        test_packages = @($targets)
        clippy_packages = @($targets)
        docs_packages = @($targets)
    }
}

$workspaceRootPath = (Resolve-Path $WorkspaceRoot).Path
if ($RepositoryRoot) {
    $repoRootPath = (Resolve-Path $RepositoryRoot).Path
} else {
    $gitRoot = & git -C $workspaceRootPath rev-parse --show-toplevel 2>$null
    $gitExitCode = $LASTEXITCODE
    $global:LASTEXITCODE = 0
    if ($gitExitCode -ne 0 -or -not $gitRoot) {
        throw "RepositoryRoot was not supplied and git could not resolve the repository containing '$workspaceRootPath'."
    }
    $repoRootPath = [System.IO.Path]::GetFullPath(($gitRoot | Select-Object -First 1).Trim())
}

$workspaceRelative = [System.IO.Path]::GetRelativePath($repoRootPath, $workspaceRootPath)
$workspacePrefix = Convert-ToRepositoryPath $workspaceRelative
if ($workspacePrefix -eq ".") {
    $workspacePrefix = ""
}
if ($workspacePrefix -eq ".." -or $workspacePrefix.StartsWith("../")) {
    throw "WorkspaceRoot '$workspaceRootPath' must be inside RepositoryRoot '$repoRootPath'."
}

$docsExact = @(
    (Join-RepositoryPath $workspacePrefix "AGENTS.md"),
    (Join-RepositoryPath $workspacePrefix "CLAUDE.md"),
    (Join-RepositoryPath $workspacePrefix "README.md")
)
$docsPrefix = Join-RepositoryPath $workspacePrefix "docs/"
$fullExact = @(
    ".github/workflows/gameengine-windows-validation.yml",
    ".github/workflows/gameengine-chatgpt-dispatcher.yml",
    ".github/workflows/gameengine-chatgpt-dispatch-trigger.yml",
    (Join-RepositoryPath $workspacePrefix "Cargo.toml"),
    (Join-RepositoryPath $workspacePrefix "Cargo.lock"),
    (Join-RepositoryPath $workspacePrefix "rust-toolchain.toml"),
    (Join-RepositoryPath $workspacePrefix ".gitattributes")
)
$fullPrefixes = @(
    (Join-RepositoryPath $workspacePrefix ".cargo/"),
    (Join-RepositoryPath $workspacePrefix "scripts/ci/"),
    (Join-RepositoryPath $workspacePrefix "GameEngine-ChatGPT-Apply/")
)

$changedPaths = @(
    Get-Content -LiteralPath $ChangedFilesPath -Encoding utf8 |
        ForEach-Object { Convert-ToRepositoryPath $_ } |
        Where-Object { $_ }
)

if ($EventName -eq "schedule") {
    $plan = New-Plan -Mode "full" -Skip $false `
        -Reason "Nightly validation always runs the full workspace suite." `
        -ChangedPackages @() -AffectedPackages @()
} elseif ($EventName -eq "push") {
    $plan = New-Plan -Mode "full" -Skip $false `
        -Reason "Pushes to main always run the full workspace suite." `
        -ChangedPackages @() -AffectedPackages @()
} elseif ($changedPaths.Count -eq 0) {
    $plan = New-Plan -Mode "skip" -Skip $true `
        -Reason "No relevant changed paths were found." `
        -ChangedPackages @() -AffectedPackages @()
} else {
    $docsOnly = $true
    foreach ($path in $changedPaths) {
        if (
            -not ($docsExact -contains $path) -and
            -not $path.StartsWith($docsPrefix)
        ) {
            $docsOnly = $false
            break
        }
    }

    if ($docsOnly) {
        $plan = New-Plan -Mode "docs" -Skip $true `
            -Reason "Documentation-only change; Rust compilation is skipped." `
            -ChangedPackages @() -AffectedPackages @()
    } else {
        $forceFullPath = $null
        foreach ($path in $changedPaths) {
            if (
                ($fullExact -contains $path) -or
                ($fullPrefixes | Where-Object { $path.StartsWith($_) } | Select-Object -First 1)
            ) {
                $forceFullPath = $path
                break
            }
        }

        $metadata = $null
        if ($forceFullPath) {
            $plan = New-Plan -Mode "full" -Skip $false `
                -Reason "Full validation selected because '$forceFullPath' has workspace-wide or uncertain impact." `
                -ChangedPackages @() -AffectedPackages @()
        } else {
            try {
                if ($MetadataFile) {
                    $metadata = Get-Content -Raw -LiteralPath $MetadataFile -Encoding utf8 | ConvertFrom-Json -AsHashtable
                } else {
                    Push-Location $workspaceRootPath
                    $rustcWrapper = $env:RUSTC_WRAPPER
                    $metadataErrorPath = Join-Path ([System.IO.Path]::GetTempPath()) ("gameengine-metadata-" + [guid]::NewGuid().ToString("N") + ".log")
                    try {
                        Remove-Item Env:RUSTC_WRAPPER -ErrorAction SilentlyContinue
                        $metadataText = & cargo metadata --format-version 1 --locked 2>$metadataErrorPath
                        $metadataExitCode = $LASTEXITCODE
                        $global:LASTEXITCODE = 0
                        if ($metadataExitCode -ne 0) {
                            $metadataError = (Get-Content -Raw -LiteralPath $metadataErrorPath -ErrorAction SilentlyContinue).Trim()
                            throw "cargo metadata failed with exit code $metadataExitCode. $metadataError"
                        }
                        $metadata = $metadataText | ConvertFrom-Json -AsHashtable
                    } finally {
                        Remove-Item -LiteralPath $metadataErrorPath -Force -ErrorAction SilentlyContinue
                        if ($null -ne $rustcWrapper) {
                            $env:RUSTC_WRAPPER = $rustcWrapper
                        }
                        Pop-Location
                    }
                }
            } catch {
                $global:LASTEXITCODE = 0
                $metadataFailure = (($_.Exception.Message -replace "\r?\n", " ") -replace "\s+", " ").Trim()
                $plan = New-Plan -Mode "full" -Skip $false `
                    -Reason "cargo metadata could not be evaluated safely: $metadataFailure" `
                    -ChangedPackages @() -AffectedPackages @()
            }
        }

        if (-not $plan) {
            $workspaceMemberIds = @{}
            foreach ($id in $metadata.workspace_members) {
                $workspaceMemberIds[[string]$id] = $true
            }

            $packagesById = @{}
            $packageRoots = @()
            foreach ($package in $metadata.packages) {
                $id = [string]$package.id
                if (-not $workspaceMemberIds.ContainsKey($id)) {
                    continue
                }

                $manifest = [System.IO.Path]::GetFullPath([string]$package.manifest_path)
                $packageRoot = Split-Path -Parent $manifest
                $relativeRoot = [System.IO.Path]::GetRelativePath($repoRootPath, $packageRoot)
                $relativeRoot = Convert-ToRepositoryPath $relativeRoot

                $record = [pscustomobject]@{
                    id = $id
                    name = [string]$package.name
                    root = $relativeRoot.TrimEnd("/")
                }
                $packagesById[$id] = $record
                $packageRoots += $record
            }
            $packageRoots = @($packageRoots | Sort-Object { $_.root.Length } -Descending)

            $changedIds = New-Object System.Collections.Generic.HashSet[string]
            $unknownPath = $null
            foreach ($path in $changedPaths) {
                if ($fullExact -contains $path) {
                    continue
                }
                if ($docsExact -contains $path -or $path.StartsWith($docsPrefix)) {
                    continue
                }

                $owner = $null
                foreach ($candidate in $packageRoots) {
                    if ($path -eq "$($candidate.root)/Cargo.toml" -or $path.StartsWith("$($candidate.root)/")) {
                        $owner = $candidate
                        break
                    }
                }

                if ($owner) {
                    [void]$changedIds.Add($owner.id)
                } elseif (-not $forceFullPath) {
                    $unknownPath = $path
                    break
                }
            }

            if ($forceFullPath -or $unknownPath) {
                $trigger = if ($forceFullPath) { $forceFullPath } else { $unknownPath }
                $plan = New-Plan -Mode "full" -Skip $false `
                    -Reason "Full validation selected because '$trigger' has workspace-wide or uncertain impact." `
                    -ChangedPackages @($changedIds | ForEach-Object { $packagesById[$_].name }) `
                    -AffectedPackages @()
            } elseif ($changedIds.Count -eq 0) {
                $plan = New-Plan -Mode "skip" -Skip $true `
                    -Reason "No Rust workspace package is affected." `
                    -ChangedPackages @() -AffectedPackages @()
            } else {
                $changedNames = @($changedIds | ForEach-Object { $packagesById[$_].name })
                $plan = New-Plan -Mode "affected" -Skip $false `
                    -Reason "Changed workspace packages were selected from cargo metadata; reverse-dependent coverage runs on main and nightly." `
                    -ChangedPackages $changedNames -AffectedPackages $changedNames
            }
        }
    }
}

$json = $plan | ConvertTo-Json -Depth 8
if ($OutputPath) {
    $parent = Split-Path -Parent $OutputPath
    if ($parent) {
        New-Item -ItemType Directory -Force -Path $parent | Out-Null
    }
    Set-Content -LiteralPath $OutputPath -Value $json -Encoding utf8
}
$json
