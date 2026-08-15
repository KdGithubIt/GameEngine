$ErrorActionPreference = "Stop"

$engineRoot = Resolve-Path (Join-Path $PSScriptRoot "..")
Push-Location $engineRoot

try {
    Write-Host "==> cargo fmt --all --check"
    & cargo fmt --all --check
    if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }

    Write-Host "==> cargo clippy --workspace --all-targets -- -D warnings"
    & cargo clippy --workspace --all-targets -- -D warnings
    if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }

    Write-Host "==> cargo test --workspace"
    & cargo test --workspace
    if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
}
finally {
    Pop-Location
}
