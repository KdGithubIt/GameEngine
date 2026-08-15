@echo off
setlocal

cd /d "%~dp0"

if not exist "Cargo.toml" (
    echo [ERROR] Cargo.toml が見つかりません。
    echo このファイルを GameEngine フォルダ直下に置いてください。
    echo.
    pause
    exit /b 1
)

where cargo >nul 2>nul
if errorlevel 1 (
    echo [ERROR] cargo が見つかりません。
    echo Rust がインストールされ、cargo に PATH が通っているか確認してください。
    echo.
    pause
    exit /b 1
)

echo ========================================
echo engine-editor を最新ソースからビルドして起動します
echo ========================================
echo.

cargo run -p engine-editor --bin engine-editor

if errorlevel 1 (
    echo.
    echo [ERROR] ビルドまたは起動に失敗しました。
    echo 上のエラー内容を確認してください。
    echo.
    pause
    exit /b 1
)

endlocal
