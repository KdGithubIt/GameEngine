[CmdletBinding()]
param(
    [string]$Planner = (Join-Path $PSScriptRoot "New-ValidationPlan.ps1")
)

$ErrorActionPreference = "Stop"
$temp = Join-Path ([System.IO.Path]::GetTempPath()) ("gameengine-plan-tests-" + [guid]::NewGuid().ToString("N"))

function New-TestMetadata {
    param(
        [string]$Workspace,
        [string]$MetadataPath
    )

    function Package {
        param([string]$Name)
        $id = "$Name 0.1.0 (path+file:///$Name)"
        return [ordered]@{
            name = $Name
            id = $id
            manifest_path = (Join-Path $Workspace "crates/$Name/Cargo.toml")
        }
    }

    $packages = @(
        (Package "leaf"),
        (Package "base"),
        (Package "mid"),
        (Package "top"),
        (Package "newcrate")
    )
    $id = @{}
    foreach ($package in $packages) { $id[$package.name] = $package.id }

    $metadata = [ordered]@{
        packages = $packages
        workspace_members = @($packages.id)
        resolve = [ordered]@{
            nodes = @(
                [ordered]@{ id = $id.leaf; dependencies = @() },
                [ordered]@{ id = $id.base; dependencies = @() },
                [ordered]@{ id = $id.mid; dependencies = @($id.base) },
                [ordered]@{ id = $id.top; dependencies = @($id.mid) },
                [ordered]@{ id = $id.newcrate; dependencies = @() }
            )
        }
    }
    $metadata | ConvertTo-Json -Depth 8 | Set-Content -LiteralPath $MetadataPath -Encoding utf8
}

function Invoke-LayoutTests {
    param(
        [string]$LayoutName,
        [string]$Repository,
        [string]$Workspace,
        [string]$Prefix
    )

    New-Item -ItemType Directory -Force -Path $Workspace | Out-Null
    $metadataPath = Join-Path $temp "$LayoutName.metadata.json"
    New-TestMetadata -Workspace $Workspace -MetadataPath $metadataPath

    function Path-InWorkspace {
        param([string]$Path)
        if (-not $Prefix) { return $Path }
        return "$Prefix/$Path"
    }

    function Assert-Plan {
        param(
            [string]$Name,
            [string]$Event,
            [string[]]$Paths,
            [string]$ExpectedMode,
            [string[]]$ExpectedAffected = @()
        )
        $changed = Join-Path $temp "$LayoutName-$Name.changed.txt"
        $Paths | Set-Content -LiteralPath $changed -Encoding utf8
        $json = & $Planner `
            -EventName $Event `
            -ChangedFilesPath $changed `
            -WorkspaceRoot $Workspace `
            -RepositoryRoot $Repository `
            -MetadataFile $metadataPath
        $plan = $json | ConvertFrom-Json
        if ($plan.validation_mode -ne $ExpectedMode) {
            throw "$LayoutName/$Name expected mode $ExpectedMode, got $($plan.validation_mode)."
        }
        $actual = @($plan.affected_packages | Sort-Object)
        $expected = @($ExpectedAffected | Sort-Object)
        if (($actual -join ",") -ne ($expected -join ",")) {
            throw "$LayoutName/$Name expected affected '$($expected -join ",")', got '$($actual -join ",")'."
        }
        Write-Host "[PASS] $LayoutName/$Name -> $ExpectedMode [$($actual -join ", ")]"
    }

    Assert-Plan "docs-only" "pull_request" @((Path-InWorkspace "docs/README.md")) "docs"
    Assert-Plan "leaf-change" "pull_request" @((Path-InWorkspace "crates/leaf/src/lib.rs")) "affected" @("leaf")
    Assert-Plan "dependent-chain-fast-path" "pull_request" @((Path-InWorkspace "crates/base/src/lib.rs")) "affected" @("base")
    Assert-Plan "multiple-packages" "pull_request" @((Path-InWorkspace "crates/base/src/lib.rs"), (Path-InWorkspace "crates/leaf/src/lib.rs")) "affected" @("base", "leaf")
    Assert-Plan "crate-addition" "pull_request" @((Path-InWorkspace "crates/newcrate/src/lib.rs"), (Path-InWorkspace "crates/newcrate/Cargo.toml")) "affected" @("newcrate")
    Assert-Plan "crate-deletion" "pull_request" @((Path-InWorkspace "crates/removed/src/lib.rs")) "full"
    Assert-Plan "package-manifest" "pull_request" @((Path-InWorkspace "crates/base/Cargo.toml")) "affected" @("base")
    Assert-Plan "workspace-manifest" "pull_request" @((Path-InWorkspace "Cargo.toml")) "full"
    Assert-Plan "cargo-lock" "pull_request" @((Path-InWorkspace "Cargo.lock")) "full"
    Assert-Plan "ci-workflow" "pull_request" @(".github/workflows/gameengine-windows-validation.yml") "full"
    Assert-Plan "nightly" "schedule" @((Path-InWorkspace "crates/leaf/src/lib.rs")) "full"
    Assert-Plan "main-push" "push" @((Path-InWorkspace "crates/leaf/src/lib.rs")) "full"
    Assert-Plan "pull-request" "pull_request" @((Path-InWorkspace "crates/leaf/src/lib.rs")) "affected" @("leaf")
    Assert-Plan "merge-group" "merge_group" @((Path-InWorkspace "crates/leaf/src/lib.rs")) "affected" @("leaf")
    Assert-Plan "dispatcher" "workflow_dispatch" @((Path-InWorkspace "crates/leaf/src/lib.rs")) "affected" @("leaf")

    $forcedChanged = Join-Path $temp "$LayoutName-ci-script-short-circuit.changed.txt"
    (Path-InWorkspace "scripts/ci/example.ps1") | Set-Content -LiteralPath $forcedChanged -Encoding utf8
    $missingMetadata = Join-Path $temp "$LayoutName-missing.metadata.json"
    $forcedJson = & $Planner `
        -EventName "pull_request" `
        -ChangedFilesPath $forcedChanged `
        -WorkspaceRoot $Workspace `
        -RepositoryRoot $Repository `
        -MetadataFile $missingMetadata
    $forcedPlan = $forcedJson | ConvertFrom-Json
    if ($forcedPlan.validation_mode -ne "full") {
        throw "$LayoutName/ci-script-short-circuit expected mode full, got $($forcedPlan.validation_mode)."
    }
    Write-Host "[PASS] $LayoutName/ci-script-short-circuit -> full without metadata"

    $malformedMetadata = Join-Path $temp "$LayoutName-malformed.metadata.json"
    "{" | Set-Content -LiteralPath $malformedMetadata -Encoding utf8
    $metadataFailureChanged = Join-Path $temp "$LayoutName-metadata-failure.changed.txt"
    (Path-InWorkspace "crates/leaf/src/lib.rs") | Set-Content -LiteralPath $metadataFailureChanged -Encoding utf8
    $failureJson = & $Planner `
        -EventName "pull_request" `
        -ChangedFilesPath $metadataFailureChanged `
        -WorkspaceRoot $Workspace `
        -RepositoryRoot $Repository `
        -MetadataFile $malformedMetadata
    $failurePlan = $failureJson | ConvertFrom-Json
    if ($failurePlan.validation_mode -ne "full") {
        throw "$LayoutName/metadata-failure expected mode full, got $($failurePlan.validation_mode)."
    }
    if ($failurePlan.reason -match "[`r`n]") {
        throw "$LayoutName/metadata-failure returned a multiline scope reason."
    }
    Write-Host "[PASS] $LayoutName/metadata-failure -> single-line full fallback"
}

try {
    $nestedRepo = Join-Path $temp "nested-repo"
    Invoke-LayoutTests `
        -LayoutName "nested" `
        -Repository $nestedRepo `
        -Workspace (Join-Path $nestedRepo "GameEngine") `
        -Prefix "GameEngine"

    $standaloneRepo = Join-Path $temp "standalone-repo"
    Invoke-LayoutTests `
        -LayoutName "standalone" `
        -Repository $standaloneRepo `
        -Workspace $standaloneRepo `
        -Prefix ""

    # The current private workflow already executes this planner regression suite.
    # Running the snapshot boundary test here keeps migration tooling covered before
    # the standalone public workflow exists.
    $snapshotTest = Join-Path $PSScriptRoot "../public/Test-PublicSnapshot.ps1"
    & $snapshotTest
} finally {
    Remove-Item -Recurse -Force $temp -ErrorAction SilentlyContinue
}
