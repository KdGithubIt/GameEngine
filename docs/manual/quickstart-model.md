# クイックガイド: モデルを表示する

対象: エディタでシーンに 3D モデルを出したい人。詳細は `docs/USER_MANUAL_JA.md` §9 を参照。

## OBJ / glTF / FBX を表示するまで

1. **ファイルを入れる**: OBJ / glTF / GLB / FBX を Asset Browser へドラッグ&ドロップする
   （またはエクスプローラで `assets/` 以下へコピーして Refresh）。
   FBX は `ufbx` 経由で直接読める（ADR 0081）。ブレンドシェイプなど未対応の機能を
   使う場合のみ DCC ツールで glTF 2.0 に変換する（`docs/FBX_IMPORT.md`）。
2. **登録する**: ファイルを右クリック → **Register Asset**。
   glTF／FBX はどちらもバックグラウンドインポートが走り、メッシュ / マテリアル /
   テクスチャ / スキン / アニメーションクリップがサブアセットとして Manifest に
   登録される。選択するとサブアセット一覧がブラウザ下部に表示される。
3. **シーンに置く**（いずれか）:
   - メッシュアセット（またはサブアセット行）を **Scene View にドラッグ** → entity が生成される
   - メッシュを右クリック → **Add to Scene**
   - 既存 entity に **Add Component → Mesh** を付け、ピッカーかドラッグでアセットを割り当てる
4. **マテリアル**: entity に **Material** コンポーネントを付けて割り当てる。
   標準マテリアルは Asset Browser 右クリック → **Create → Material** で作成し、
   Material Editor で色やテクスチャを設定する。
   マテリアルは **Scene View 上のオブジェクトへ直接ドロップ**しても割り当てられる。

## うまくいかないとき

- Add Component の一覧に出ない → 対象 entity が既にそのコンポーネントを持っている
- ピッカーに出ない → Register Asset を忘れている（ブラウザのタイルに ✓ が付いているか確認）
- 表示されない → Console にエラーが出ていないか確認（Clear / Copy All あり）
