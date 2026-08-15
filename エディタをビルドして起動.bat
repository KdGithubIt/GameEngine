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
    echo Rust をインストールして、cargo に PATH が通っているか確認してください。
    echo.
    pause
    exit /b 1
)

echo ========================================
echo GameEngine ランチャーを最新ソースからビルドして起動します
echo ========================================
echo.

rem ランチャーは自分と同じフォルダに置かれた engine-editor を起動するため、
rem エディタとランチャーを同じプロファイルでまとめてビルドします。
cargo build -p engine-editor -p engine-launcher

if errorlevel 1 (
    echo.
    echo [ERROR] ビルドに失敗しました。
    echo 上のエラー内容を確認してください。
    echo.
    pause
    exit /b 1
)

rem プロジェクトの選択と新規作成はランチャーが担当します。
rem エディタはランチャーから --project 付きで起動されます。
cargo run -p engine-launcher --bin engine-launcher

if errorlevel 1 (
    echo.
    echo [ERROR] ランチャーの起動に失敗しました。
    echo 上のエラー内容を確認してください。
    echo.
    pause
    exit /b 1
)

endlocal
