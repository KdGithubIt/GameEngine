@echo off

rem このバッチは、GameEngineのエディタをCargoのreleaseプロファイルで
rem ビルドし、ビルドに成功した実行ファイルをそのまま起動するためのものです。
rem 既存の通常ビルド版バッチは変更せず、用途に応じて使い分けられるようにします。
setlocal

rem エクスプローラーなど、プロジェクト外の作業フォルダから起動された場合でも
rem Cargo.tomlを正しく見つけられるよう、バッチ自身が置かれたフォルダへ移動します。
cd /d "%~dp0"

rem このバッチがGameEngineのルート以外へ移動・コピーされてしまった場合に、
rem 関係のないフォルダでCargoを実行しないよう、Cargo.tomlの存在を確認します。
if not exist "Cargo.toml" (
    echo [ERROR] Cargo.toml が見つかりません。
    echo このファイルを GameEngine フォルダ直下に置いてください。
    echo.
    rem ダブルクリック起動時にもエラー内容を読めるよう、ウィンドウを閉じずに待機します。
    pause
    exit /b 1
)

rem Rustツールチェーンが未インストール、またはcargoにPATHが通っていない場合は、
rem ビルドを開始せず、利用者が原因を判断できるメッセージを表示して終了します。
where cargo >nul 2>nul
if errorlevel 1 (
    echo [ERROR] cargo が見つかりません。
    echo Rust がインストールされ、cargo に PATH が通っているか確認してください。
    echo.
    rem エラー表示がすぐ閉じてしまわないよう、利用者のキー入力を待ちます。
    pause
    exit /b 1
)

rem ここからreleaseプロファイルによるエディタのビルドと起動を開始します。
rem 初回やソース更新後は最適化処理が行われるため、通常ビルドより時間がかかる場合があります。
echo ========================================
echo engine-editor をリリースビルドして起動します
echo ========================================
echo.

rem --releaseにより最適化されたreleaseプロファイルを選択します。
rem -pと--binで、ワークスペース内のengine-editor実行バイナリを明示的に指定します。
rem ビルドに成功すると、cargo runが生成されたエディタを続けて起動します。
cargo run --release -p engine-editor --bin engine-editor

rem Cargoの終了コードが0以外なら、コンパイルエラーまたはエディタ起動時の
rem 異常終了として扱い、コンソールに出力された詳細を確認できる状態で停止します。
if errorlevel 1 (
    echo.
    echo [ERROR] リリースビルドまたは起動に失敗しました。
    echo 上のエラー内容を確認してください。
    echo.
    pause
    exit /b 1
)

rem 正常終了時に、このバッチ内で設定したローカル環境を破棄します。
endlocal
