# クイックガイド: 画像を使う

このエンジンに「画像を 1 枚だけ置くコンポーネント」はない。画像の用途は 2 つ。

## 1. 3D 表面のテクスチャとして（マテリアル経由）

1. PNG / JPG / WebP / BMP を Asset Browser に入れて **Register Asset**
2. マテリアルを開く（無ければ右クリック → **Create → Material**）
3. Material Editor で **Base color texture**（必要なら Normal / Emissive）に割り当てる
4. そのマテリアルを entity の **Material** コンポーネントに割り当てる
   （Scene View のオブジェクトへ直接ドロップ可）

## 2. 画面 UI の画像として（UI Document 経由）

1. Asset Browser 右クリック → **Create → UI Document**（または既存の `*.ui.json` を開く）
2. UI Builder のパレットから **Image** を追加する
3. **Image Source** の **Pick…** で登録済みテクスチャを選ぶ
   （`assets/` からの相対パスが入る。プレビューには実画像が表示され、
   ファイルを差し替えると自動で再読込される）
4. シーンで使うには entity に **UI Document** コンポーネントを付けて割り当てる

## 補足

- 9-slice（枠を伸ばさない拡大）は Image ノードの nine_slice で指定でき、
  ビルダーのプレビューはランタイムと同じ描画コードを使う
- HUD の数値などは Text ノードの Binding を使う（UI Builder の
  **Preview Bindings** にテスト値を入れるとレイアウト確認ができる）
