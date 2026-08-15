@echo off

rem このバッチは、GameEngine のランチャーとエディタを Cargo の release プロファイルで
rem ビルドし、ビルドに成功したらランチャーをそのまま起動するためのものです。
rem 隣の通常ビルド用バッチは変更せず、用途に応じて使い分けられるようにします。
setlocal

rem エクスプローラーなど、プロジェクト外の作業フォルダから起動された場合でも
rem Cargo.toml を正しく解決できるよう、バッチが置かれたフォルダへ移動します。
cd /d "%~dp0"

rem このバッチが GameEngine のルート以外へ移動・コピーされてしまった場合に、
rem 関係のないフォルダで Cargo を実行しないよう、Cargo.toml の存在を確認します。
if not exist "Cargo.toml" (
    echo [ERROR] Cargo.toml が見つかりません。
    echo このファイルを GameEngine フォルダ直下に置いてください。
    echo.
    rem ダブルクリック起動時にもエラー内容を読めるよう、ウィンドウを閉じずに待機します。
    pause
    exit /b 1
)

rem Rust ツールチェーンが未インストール、または cargo に PATH が通っていない場合は、
rem ビルドを開始せず、利用者が原因を判断できるメッセージを表示して終了します。
where cargo >nul 2>nul
if errorlevel 1 (
    echo [ERROR] cargo が見つかりません。
    echo Rust をインストールして、cargo に PATH が通っているか確認してください。
    echo.
    rem エラー表示が流れてしまわないよう、利用者のキー入力を待ちます。
    pause
    exit /b 1
)

rem ここから release プロファイルによるビルドとランチャーの起動を開始します。
rem 初回や大きなソース更新後は最適化処理が走るため、通常ビルドより時間がかかる場合があります。
echo ========================================
echo GameEngine ランチャーをリリースビルドして起動します
echo ========================================
echo.

rem ランチャーは自分と同じフォルダに置かれた engine-editor を起動します。
rem そのため、エディタとランチャーを同じ release プロファイルでまとめてビルドします。
cargo build --release -p engine-editor -p engine-launcher

rem ビルドが失敗した場合は起動へ進まず、コンパイルエラーを確認できる状態で停止します。
if errorlevel 1 (
    echo.
    echo [ERROR] リリースビルドに失敗しました。
    echo 上のエラー内容を確認してください。
    echo.
    pause
    exit /b 1
)

rem プロジェクトの選択と新規作成はランチャーが担当します。
rem エディタは編集対象のプロジェクトをランチャーから --project 付きで受け取って起動します。
cargo run --release -p engine-launcher --bin engine-launcher

rem Cargo の終了コードが 0 以外なら、ランチャーの起動失敗または異常終了として扱い、
rem コンソールに出力された詳細を確認できる状態で停止します。
if errorlevel 1 (
    echo.
    echo [ERROR] ランチャーの起動に失敗しました。
    echo 上のエラー内容を確認してください。
    echo.
    pause
    exit /b 1
)

rem 正常終了時に、このバッチ内で設定したローカル環境を破棄します。
endlocal
