[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)]
    [string]$PlanPath,

    [string]$WorkspaceRoot = (Resolve-Path (Join-Path $PSScriptRoot "../..")).Path,
    [string]$DiagnosticsDirectory = "",

    [ValidateSet("all", "lint", "tests", "documentation")]
    [string]$Gate = "all"
)

$ErrorActionPreference = "Stop"
$plan = Get-Content -Raw -LiteralPath $PlanPath -Encoding utf8 | ConvertFrom-Json

if ($plan.skip) {
    Write-Host "Validation plan is skippable: $($plan.reason)"
    exit 0
}

if (-not $DiagnosticsDirectory) {
    $DiagnosticsDirectory = Join-Path ([System.IO.Path]::GetTempPath()) "gameengine-ci-diagnostics"
}
New-Item -ItemType Directory -Force -Path $DiagnosticsDirectory | Out-Null

function Invoke-CargoGate {
    param(
        [Parameter(Mandatory = $true)]
        [string]$Name,
        [Parameter(Mandatory = $true)]
        [string[]]$Arguments
    )

    Write-Host "==> cargo $($Arguments -join ' ')"
    & cargo @Arguments
    if ($LASTEXITCODE -ne 0) {
        $diagnostic = [ordered]@{
            gate = $Name
            command = "cargo $($Arguments -join ' ')"
            exit_code = $LASTEXITCODE
            utc = [DateTime]::UtcNow.ToString("o")
        } | ConvertTo-Json
        Set-Content -LiteralPath (Join-Path $DiagnosticsDirectory "$Name.json") -Value $diagnostic -Encoding utf8
        throw "$Name failed with exit code $LASTEXITCODE."
    }
}

function Get-PackageArgs {
    param([object[]]$Packages)
    $args = @()
    foreach ($package in $Packages) {
        if ($package) {
            $args += @("-p", [string]$package)
        }
    }
    return $args
}

function Invoke-LintGate {
    Invoke-CargoGate -Name "formatting" -Arguments @("fmt", "--all", "--check")

    if ($plan.validation_mode -eq "full") {
        Invoke-CargoGate -Name "clippy" -Arguments @("clippy", "--workspace", "--all-targets", "--", "-D", "warnings")
    } elseif ($plan.validation_mode -eq "affected") {
        $clippyArgs = @("clippy") + (Get-PackageArgs $plan.clippy_packages) + @("--", "-D", "warnings")
        Invoke-CargoGate -Name "clippy" -Arguments $clippyArgs
    } else {
        throw "Unsupported validation mode '$($plan.validation_mode)'."
    }
}

function Invoke-TestGate {
    if ($plan.validation_mode -eq "full") {
        Invoke-CargoGate -Name "tests" -Arguments @("test", "--workspace")
    } elseif ($plan.validation_mode -eq "affected") {
        $testArgs = @("test") + (Get-PackageArgs $plan.test_packages)
        Invoke-CargoGate -Name "tests" -Arguments $testArgs
    } else {
        throw "Unsupported validation mode '$($plan.validation_mode)'."
    }
}

function Invoke-DocumentationGate {
    if ($plan.validation_mode -eq "full") {
        Invoke-CargoGate -Name "documentation" -Arguments @("doc", "--workspace", "--no-deps")
    } elseif ($plan.validation_mode -eq "affected") {
        $docArgs = @("doc") + (Get-PackageArgs $plan.docs_packages) + @("--no-deps")
        Invoke-CargoGate -Name "documentation" -Arguments $docArgs
    } else {
        throw "Unsupported validation mode '$($plan.validation_mode)'."
    }
}

Push-Location $WorkspaceRoot
try {
    Write-Host "Executing validation gate '$Gate' in mode '$($plan.validation_mode)'."

    if ($Gate -eq "all" -or $Gate -eq "lint") {
        Invoke-LintGate
    }
    if ($Gate -eq "all" -or $Gate -eq "tests") {
        Invoke-TestGate
    }
    if ($Gate -eq "all" -or $Gate -eq "documentation") {
        Invoke-DocumentationGate
    }
} finally {
    Pop-Location
}
