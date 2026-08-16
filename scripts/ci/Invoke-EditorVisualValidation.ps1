[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)]
    [ValidateSet("editor", "launcher", "both")]
    [string]$Target,

    [string]$ProjectPath = "",

    [string]$AuthoringTool = "",

    [string]$WorkspaceRoot = (Resolve-Path (Join-Path $PSScriptRoot "../..")).Path,

    [string]$OutputDirectory = ""
)

$ErrorActionPreference = "Stop"
$workspace = (Resolve-Path -LiteralPath $WorkspaceRoot).Path

if ($AuthoringTool -and $Target -eq "launcher") {
    throw "AuthoringTool requires an Editor capture target."
}

if (-not $OutputDirectory) {
    $OutputDirectory = Join-Path ([System.IO.Path]::GetTempPath()) "gameengine-editor-visual-validation"
}
$OutputDirectory = [System.IO.Path]::GetFullPath($OutputDirectory)
New-Item -ItemType Directory -Force -Path $OutputDirectory | Out-Null

function Invoke-CargoChecked {
    param(
        [Parameter(Mandatory = $true)]
        [string[]]$Arguments
    )

    Write-Host "==> cargo $($Arguments -join ' ')"
    & cargo @Arguments | Out-Host
    if ($LASTEXITCODE -ne 0) {
        throw "cargo $($Arguments -join ' ') failed with exit code $LASTEXITCODE."
    }
}

function Resolve-RepositoryProject {
    param([Parameter(Mandatory = $true)][string]$RelativePath)

    if ([System.IO.Path]::IsPathRooted($RelativePath)) {
        throw "ProjectPath must be relative to the workspace root."
    }

    $candidate = [System.IO.Path]::GetFullPath((Join-Path $workspace $RelativePath))
    $workspacePrefix = $workspace.TrimEnd(
        [System.IO.Path]::DirectorySeparatorChar,
        [System.IO.Path]::AltDirectorySeparatorChar
    ) + [System.IO.Path]::DirectorySeparatorChar
    if (-not $candidate.StartsWith($workspacePrefix, [System.StringComparison]::OrdinalIgnoreCase)) {
        throw "ProjectPath must stay inside the workspace root."
    }
    if (-not (Test-Path -LiteralPath $candidate -PathType Container)) {
        throw "Visual-validation project does not exist: $candidate"
    }
    if (-not (Test-Path -LiteralPath (Join-Path $candidate "project.json") -PathType Leaf)) {
        throw "Visual-validation project does not contain project.json: $candidate"
    }

    return (Resolve-Path -LiteralPath $candidate).Path
}

function New-StandardVisualValidationProject {
    $temporaryRoot = Join-Path ([System.IO.Path]::GetTempPath()) (
        "gameengine-visual-project-" + [Guid]::NewGuid().ToString("N")
    )
    $helperRoot = Join-Path $temporaryRoot "helper"
    $helperSource = Join-Path $helperRoot "src"
    $projectRoot = Join-Path $temporaryRoot "project"
    New-Item -ItemType Directory -Force -Path $helperSource | Out-Null

    $lifecyclePath = (Join-Path $workspace "crates/project-lifecycle").Replace("\", "/")
    $manifest = @"
[package]
name = "gameengine-visual-project-helper"
version = "0.0.0"
edition = "2024"
publish = false

[workspace]

[dependencies]
engine-project-lifecycle = { path = "$lifecyclePath" }
"@
    $source = @'
use std::error::Error;
use std::io;
use std::path::Path;

fn main() -> Result<(), Box<dyn Error>> {
    let project = std::env::args_os()
        .nth(1)
        .ok_or_else(|| io::Error::other("missing visual-validation project path"))?;
    engine_project_lifecycle::create_standard_project(Path::new(&project), "VisualValidation")?;
    Ok(())
}
'@

    $manifestPath = Join-Path $helperRoot "Cargo.toml"
    Set-Content -LiteralPath $manifestPath -Value $manifest -Encoding utf8
    Set-Content -LiteralPath (Join-Path $helperSource "main.rs") -Value $source -Encoding utf8
    Invoke-CargoChecked -Arguments @(
        "run",
        "--quiet",
        "--manifest-path",
        $manifestPath,
        "--",
        $projectRoot
    )

    return $projectRoot
}

function Invoke-DesktopScreenshot {
    param(
        [Parameter(Mandatory = $true)][string]$Name,
        [Parameter(Mandatory = $true)][string]$Package,
        [Parameter(Mandatory = $true)]
        [AllowEmptyCollection()]
        [string[]]$ProgramArguments,
        [string]$RequestedAuthoringTool = ""
    )

    $outputPath = Join-Path $OutputDirectory "$Name.png"
    Remove-Item -LiteralPath $outputPath -Force -ErrorAction SilentlyContinue

    $captureVariable = if ($Package -eq "engine-editor") {
        "GAMEENGINE_SCREENSHOT_TO"
    } else {
        "GAMEENGINE_LAUNCHER_SCREENSHOT_TO"
    }
    $previousValue = [Environment]::GetEnvironmentVariable(
        $captureVariable,
        [EnvironmentVariableTarget]::Process
    )
    $previousAuthoringTool = [Environment]::GetEnvironmentVariable(
        "GAMEENGINE_VISUAL_AUTHORING_TOOL",
        [EnvironmentVariableTarget]::Process
    )
    try {
        [Environment]::SetEnvironmentVariable(
            $captureVariable,
            $outputPath,
            [EnvironmentVariableTarget]::Process
        )
        if ($Package -eq "engine-editor" -and $RequestedAuthoringTool) {
            [Environment]::SetEnvironmentVariable(
                "GAMEENGINE_VISUAL_AUTHORING_TOOL",
                $RequestedAuthoringTool,
                [EnvironmentVariableTarget]::Process
            )
        }
        $cargoArguments = @(
            "run",
            "--locked",
            "-p",
            $Package,
            "--features",
            "visual-validation"
        )
        if ($ProgramArguments.Count -gt 0) {
            $cargoArguments += "--"
            $cargoArguments += $ProgramArguments
        }
        Invoke-CargoChecked -Arguments $cargoArguments
    } finally {
        [Environment]::SetEnvironmentVariable(
            $captureVariable,
            $previousValue,
            [EnvironmentVariableTarget]::Process
        )
        [Environment]::SetEnvironmentVariable(
            "GAMEENGINE_VISUAL_AUTHORING_TOOL",
            $previousAuthoringTool,
            [EnvironmentVariableTarget]::Process
        )
    }

    if (-not (Test-Path -LiteralPath $outputPath -PathType Leaf)) {
        throw "Visual-validation screenshot was not produced: $outputPath"
    }
    $file = Get-Item -LiteralPath $outputPath
    if ($file.Length -le 0) {
        throw "Visual-validation screenshot is empty: $outputPath"
    }

    return [ordered]@{
        name = $Name
        package = $Package
        path = $file.Name
        bytes = $file.Length
        sha256 = (Get-FileHash -LiteralPath $outputPath -Algorithm SHA256).Hash.ToLowerInvariant()
    }
}

Push-Location $workspace
try {
    $projectRoot = $null
    $projectSource = $null
    $captures = @()

    if ($Target -eq "editor" -or $Target -eq "both") {
        if ($ProjectPath) {
            $projectRoot = Resolve-RepositoryProject -RelativePath $ProjectPath
            $projectSource = "repository:$ProjectPath"
        } else {
            $projectRoot = New-StandardVisualValidationProject
            $projectSource = "generated-standard-project"
        }
        $captures += Invoke-DesktopScreenshot `
            -Name "editor" `
            -Package "engine-editor" `
            -ProgramArguments @("--project", $projectRoot) `
            -RequestedAuthoringTool $AuthoringTool
    }

    if ($Target -eq "launcher" -or $Target -eq "both") {
        $captures += Invoke-DesktopScreenshot `
            -Name "launcher" `
            -Package "engine-launcher" `
            -ProgramArguments @()
    }

    $summary = [ordered]@{
        schema_version = 2
        target = $Target
        project_source = $projectSource
        authoring_tool = if ($AuthoringTool) { $AuthoringTool } else { $null }
        generated_utc = [DateTime]::UtcNow.ToString("o")
        screenshots = @($captures)
    }
    $summary | ConvertTo-Json -Depth 6 | Set-Content `
        -LiteralPath (Join-Path $OutputDirectory "summary.json") `
        -Encoding utf8
} finally {
    Pop-Location
}
