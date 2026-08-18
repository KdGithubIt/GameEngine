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

function Test-RemoteAiStudioBrowserVisualRequested {
    if ($env:GAMEENGINE_VISUAL_REMOTE_AI_STUDIO -eq "1") {
        return $true
    }

    $eventPath = $env:GITHUB_EVENT_PATH
    if (-not $eventPath -or -not (Test-Path -LiteralPath $eventPath -PathType Leaf)) {
        return $false
    }

    $eventPayload = Get-Content -Raw -LiteralPath $eventPath -Encoding utf8 | ConvertFrom-Json
    $body = [string]$eventPayload.pull_request.body
    if (-not $body) {
        return $false
    }

    $pattern = '<!--\s*gameengine-visual-remote-ai-studio:\s*browser\s*-->'
    $markerMatches = [regex]::Matches(
        $body,
        $pattern,
        [System.Text.RegularExpressions.RegexOptions]::IgnoreCase
    )
    if ($markerMatches.Count -gt 1) {
        throw "PR body may contain at most one Remote AI Studio browser visual-validation marker."
    }
    return $markerMatches.Count -eq 1
}

function Resolve-EdgeExecutable {
    $candidates = @()
    if (${env:ProgramFiles(x86)}) {
        $candidates += Join-Path ${env:ProgramFiles(x86)} "Microsoft\Edge\Application\msedge.exe"
    }
    if ($env:ProgramFiles) {
        $candidates += Join-Path $env:ProgramFiles "Microsoft\Edge\Application\msedge.exe"
    }
    $command = Get-Command "msedge.exe" -ErrorAction SilentlyContinue
    if ($command) {
        $candidates += $command.Source
    }

    foreach ($candidate in $candidates | Select-Object -Unique) {
        if ($candidate -and (Test-Path -LiteralPath $candidate -PathType Leaf)) {
            return (Resolve-Path -LiteralPath $candidate).Path
        }
    }
    throw "Remote AI Studio browser visual validation requires Microsoft Edge on the Windows runner."
}

function Invoke-RemoteBrowserScreenshot {
    param(
        [Parameter(Mandatory = $true)][string]$Name,
        [Parameter(Mandatory = $true)][string]$EdgeExecutable,
        [Parameter(Mandatory = $true)][string]$Url,
        [Parameter(Mandatory = $true)][int]$Width,
        [Parameter(Mandatory = $true)][int]$Height
    )

    $outputPath = Join-Path $OutputDirectory "$Name.png"
    Remove-Item -LiteralPath $outputPath -Force -ErrorAction SilentlyContinue
    $profileRoot = Join-Path ([System.IO.Path]::GetTempPath()) (
        "gameengine-edge-visual-" + [Guid]::NewGuid().ToString("N")
    )
    New-Item -ItemType Directory -Force -Path $profileRoot | Out-Null
    $arguments = @(
        "--headless=new",
        "--disable-gpu",
        "--hide-scrollbars",
        "--no-first-run",
        "--disable-default-apps",
        "--user-data-dir=$profileRoot",
        "--window-size=$Width,$Height",
        "--virtual-time-budget=4500",
        "--screenshot=$outputPath",
        $Url
    )
    Write-Host "==> Edge Remote AI Studio capture $Name (${Width}x${Height})"
    & $EdgeExecutable @arguments | Out-Host
    if ($LASTEXITCODE -ne 0) {
        throw "Microsoft Edge screenshot '$Name' failed with exit code $LASTEXITCODE."
    }

    # Edge child processes can retain their profile briefly after the headless
    # parent exits. Each capture uses a unique runner-temp profile, so cleanup is
    # intentionally delegated to the ephemeral CI runner instead of racing it.
    if (-not (Test-Path -LiteralPath $outputPath -PathType Leaf)) {
        throw "Remote AI Studio browser screenshot was not produced: $outputPath"
    }
    $file = Get-Item -LiteralPath $outputPath
    if ($file.Length -le 0) {
        throw "Remote AI Studio browser screenshot is empty: $outputPath"
    }

    return [ordered]@{
        name = $Name
        package = "engine-editor"
        surface = "remote-ai-studio-browser"
        viewport_width = $Width
        viewport_height = $Height
        path = $file.Name
        bytes = $file.Length
        sha256 = (Get-FileHash -LiteralPath $outputPath -Algorithm SHA256).Hash.ToLowerInvariant()
    }
}

function Invoke-RemoteAiStudioBrowserScreenshots {
    param([Parameter(Mandatory = $true)][string]$ProjectRoot)

    $urlPath = Join-Path $OutputDirectory "remote-ai-studio-url.txt"
    $editorStdoutPath = Join-Path $OutputDirectory "remote-ai-studio-editor.stdout.log"
    $editorStderrPath = Join-Path $OutputDirectory "remote-ai-studio-editor.stderr.log"
    Remove-Item -LiteralPath $urlPath -Force -ErrorAction SilentlyContinue
    Remove-Item -LiteralPath $editorStdoutPath -Force -ErrorAction SilentlyContinue
    Remove-Item -LiteralPath $editorStderrPath -Force -ErrorAction SilentlyContinue
    $previousUrlPath = [Environment]::GetEnvironmentVariable(
        "GAMEENGINE_REMOTE_AI_STUDIO_VISUAL_URL_TO",
        [EnvironmentVariableTarget]::Process
    )
    $editorProcess = $null
    try {
        Invoke-CargoChecked -Arguments @(
            "build",
            "--locked",
            "-p",
            "engine-editor",
            "--features",
            "visual-validation"
        )

        $targetRoot = if ($env:CARGO_TARGET_DIR) {
            [System.IO.Path]::GetFullPath($env:CARGO_TARGET_DIR)
        } else {
            Join-Path $workspace "target"
        }
        $editorExecutable = Join-Path $targetRoot "debug\engine-editor.exe"
        if (-not (Test-Path -LiteralPath $editorExecutable -PathType Leaf)) {
            throw "Visual-validation Editor executable was not produced: $editorExecutable"
        }

        [Environment]::SetEnvironmentVariable(
            "GAMEENGINE_REMOTE_AI_STUDIO_VISUAL_URL_TO",
            $urlPath,
            [EnvironmentVariableTarget]::Process
        )
        $editorProcess = Start-Process `
            -FilePath $editorExecutable `
            -ArgumentList @("--project", "`"$ProjectRoot`"") `
            -RedirectStandardOutput $editorStdoutPath `
            -RedirectStandardError $editorStderrPath `
            -PassThru

        $companionUrl = ""
        for ($attempt = 0; $attempt -lt 300; $attempt++) {
            if (Test-Path -LiteralPath $urlPath -PathType Leaf) {
                $companionUrl = (Get-Content -Raw -LiteralPath $urlPath -Encoding utf8).Trim()
                if ($companionUrl) {
                    break
                }
            }
            if ($editorProcess.HasExited) {
                $stderr = if (Test-Path -LiteralPath $editorStderrPath -PathType Leaf) {
                    (Get-Content -Raw -LiteralPath $editorStderrPath -Encoding utf8).Trim()
                } else {
                    ""
                }
                $detail = if ($stderr) { " stderr: $stderr" } else { "" }
                throw "Editor exited before publishing the Remote AI Studio visual-validation URL (exit $($editorProcess.ExitCode)).$detail"
            }
            Start-Sleep -Milliseconds 100
        }
        if (-not $companionUrl) {
            throw "Editor did not publish the Remote AI Studio visual-validation URL within 30 seconds."
        }
        if ($companionUrl.StartsWith("ERROR:", [System.StringComparison]::Ordinal)) {
            throw $companionUrl.Substring(6).Trim()
        }
        $companionUri = $null
        if (-not [Uri]::TryCreate($companionUrl, [UriKind]::Absolute, [ref]$companionUri) -or
            ($companionUri.Scheme -ne "http" -and $companionUri.Scheme -ne "https")) {
            throw "Editor published an invalid Remote AI Studio visual-validation URL."
        }

        $edge = Resolve-EdgeExecutable
        $captures = @()
        $captures += Invoke-RemoteBrowserScreenshot -Name "remote-ai-studio-desktop" -EdgeExecutable $edge -Url $companionUrl -Width 1440 -Height 1000
        $captures += Invoke-RemoteBrowserScreenshot -Name "remote-ai-studio-desktop-full" -EdgeExecutable $edge -Url $companionUrl -Width 1440 -Height 1900
        $captures += Invoke-RemoteBrowserScreenshot -Name "remote-ai-studio-narrow" -EdgeExecutable $edge -Url $companionUrl -Width 720 -Height 1000
        $captures += Invoke-RemoteBrowserScreenshot -Name "remote-ai-studio-narrow-full" -EdgeExecutable $edge -Url $companionUrl -Width 720 -Height 2300
        $captures += Invoke-RemoteBrowserScreenshot -Name "remote-ai-studio-mobile" -EdgeExecutable $edge -Url $companionUrl -Width 390 -Height 844
        $captures += Invoke-RemoteBrowserScreenshot -Name "remote-ai-studio-mobile-full" -EdgeExecutable $edge -Url $companionUrl -Width 390 -Height 3200
        return $captures
    } finally {
        [Environment]::SetEnvironmentVariable(
            "GAMEENGINE_REMOTE_AI_STUDIO_VISUAL_URL_TO",
            $previousUrlPath,
            [EnvironmentVariableTarget]::Process
        )
        if ($editorProcess -and -not $editorProcess.HasExited) {
            & taskkill.exe /PID $editorProcess.Id /T /F | Out-Null
        }
    }
}

Push-Location $workspace
try {
    $projectRoot = $null
    $projectSource = $null
    $captures = @()
    $remoteAiStudioBrowserRequested = Test-RemoteAiStudioBrowserVisualRequested
    if ($remoteAiStudioBrowserRequested -and $Target -eq "launcher") {
        throw "Remote AI Studio browser visual validation requires an Editor capture target."
    }

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
        if ($remoteAiStudioBrowserRequested) {
            $captures += Invoke-RemoteAiStudioBrowserScreenshots -ProjectRoot $projectRoot
        }
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
        remote_ai_studio_browser = $remoteAiStudioBrowserRequested
        generated_utc = [DateTime]::UtcNow.ToString("o")
        screenshots = @($captures)
    }
    $summary | ConvertTo-Json -Depth 6 | Set-Content `
        -LiteralPath (Join-Path $OutputDirectory "summary.json") `
        -Encoding utf8
} finally {
    Pop-Location
}
