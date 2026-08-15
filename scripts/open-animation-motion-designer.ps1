$ErrorActionPreference = 'Stop'
$gameEngine = (Resolve-Path (Join-Path $PSScriptRoot '..')).Path
Push-Location $gameEngine
try {
    cargo run -p engine-editor --bin animation_motion_designer
    if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
}
finally {
    Pop-Location
}
