[CmdletBinding()]
param(
    [string]$Exporter = (Join-Path $PSScriptRoot "Export-PublicSnapshot.ps1"),
    [string]$SourceRoot = (Resolve-Path (Join-Path $PSScriptRoot "../..")).Path
)

$ErrorActionPreference = "Stop"
$temp = Join-Path ([System.IO.Path]::GetTempPath()) ("gameengine-public-snapshot-" + [guid]::NewGuid().ToString("N"))
$privatePathTemp = Join-Path ([System.IO.Path]::GetTempPath()) ("gameengine-public-snapshot-private-path-" + [guid]::NewGuid().ToString("N"))

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
        "docs/adr/0134-portable-prefab-instance-source.md",
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

    $adr0134 = Get-Content -Raw -LiteralPath (Join-Path $temp "docs/adr/0134-portable-prefab-instance-source.md") -Encoding utf8
    $placeholderPath = '\\?\C:' + '\Users\...' + '\assets\prefabs\hero.prefab.json'
    if (-not $adr0134.Contains($placeholderPath)) {
        throw "ADR 0134 no longer contains the private-user-path placeholder regression fixture."
    }

    $privatePathFixture = Join-Path $temp "docs/private-path-regression.md"
    $realPrivatePath = 'C:' + '\Users\alice\GameEngine\secrets.txt'
    "# Private path regression fixture`n`n$realPrivatePath`n" |
        Set-Content -LiteralPath $privatePathFixture -Encoding utf8

    $privatePathRejected = $false
    try {
        & $Exporter -SourceRoot $temp -DestinationRoot $privatePathTemp
    } catch {
        if ($_.Exception.Message -notmatch "private-user-path") {
            throw
        }
        $privatePathRejected = $true
    }
    if (-not $privatePathRejected) {
        throw "Expected a real Windows user-profile path to fail the public snapshot security scan."
    }

    Write-Host "[PASS] Public snapshot allow-list, asset audit, workflow set, and private-path scan are valid."
} finally {
    Remove-Item -Recurse -Force $privatePathTemp -ErrorAction SilentlyContinue
    Remove-Item -Recurse -Force $temp -ErrorAction SilentlyContinue
}
