[CmdletBinding()]
param(
    [string]$Exporter = (Join-Path $PSScriptRoot "Export-PublicSnapshot.ps1"),
    [string]$SourceRoot = (Resolve-Path (Join-Path $PSScriptRoot "../..")).Path
)

$ErrorActionPreference = "Stop"
$temp = Join-Path ([System.IO.Path]::GetTempPath()) ("gameengine-public-snapshot-" + [guid]::NewGuid().ToString("N"))

function Assert-Exists {
    param([string]$Relative)
    if (-not (Test-Path -LiteralPath (Join-Path $temp $Relative))) {
        throw "Expected public snapshot path is missing: $Relative"
    }
}

function Assert-Absent {
    param([string]$Relative)
    if (Test-Path -LiteralPath (Join-Path $temp $Relative)) {
        throw "Private/generated path leaked into public snapshot: $Relative"
    }
}

try {
    & $Exporter -SourceRoot $SourceRoot -DestinationRoot $temp

    foreach ($relative in @(
        "Cargo.toml",
        "Cargo.lock",
        "rust-toolchain.toml",
        "AGENTS.md",
        "README.md",
        "crates",
        "docs/CHATGPT_AUTOMATION.md",
        "scripts/ci/New-ValidationPlan.ps1",
        ".github/workflows/gameengine-chatgpt-dispatch-trigger.yml",
        ".github/workflows/gameengine-chatgpt-dispatcher.yml",
        ".github/workflows/gameengine-windows-validation.yml",
        ".public-snapshot-manifest.txt"
    )) {
        Assert-Exists $relative
    }

    foreach ($relative in @(
        ".codex_tmp",
        ".codex-task-15b1.md",
        ".idea",
        ".vscode",
        "GameEngine-ChatGPT-Apply",
        "build_errors.txt",
        "crates.zip",
        "docs.zip",
        "docs/GameEngine_取扱説明書_2026-07-24.pptx",
        "docs/GameEngine_操作手順・コンポーネントガイド_2026-07-26.docx"
    )) {
        Assert-Absent $relative
    }

    $workflowNames = @(
        Get-ChildItem -LiteralPath (Join-Path $temp ".github/workflows") -File |
            ForEach-Object { $_.Name } |
            Sort-Object
    )
    $expectedWorkflows = @(
        "gameengine-chatgpt-dispatch-trigger.yml",
        "gameengine-chatgpt-dispatcher.yml",
        "gameengine-windows-validation.yml"
    ) | Sort-Object
    if (($workflowNames -join ",") -ne ($expectedWorkflows -join ",")) {
        throw "Unexpected public workflow set: $($workflowNames -join ', ')"
    }

    Write-Host "[PASS] Public snapshot allow-list, asset audit, and workflow set are valid."
} finally {
    Remove-Item -Recurse -Force $temp -ErrorAction SilentlyContinue
}
