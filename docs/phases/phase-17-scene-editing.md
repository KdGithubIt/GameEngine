# Phase 17: Scene Editing / Preview の実用化

> **2026-06-13 再構成**で新設。旧 Phase 17（Collision）は
> `phase-21-collision.md` へ後ろ倒し。

## Goal

Transform / Camera / Light / Mesh / Material の編集体験を安定させ、
「編集 → Play → Stop → 編集 → 保存 → 再起動 → 開き直し」のフルループを
破綻なく回せる状態にする。

---

## Why（なぜこのフェーズが必要か・なぜこの順序か）

Phase 15 で component が出揃い、Phase 16 で外部アセットが Play に出る。
しかし「機能が存在する」と「使える」は別物で、編集ループの随所に
未確認の齟齬が残っている（例: scene 上の camera と default camera の関係、
リサイズ時の Game View、子持ち entity の削除不能）。新機能を足さずに
既存導線の穴を塞ぐフェーズを 1 つ置く。

---

## Scope

### 作るもの

- camera / light / material 編集が Play に正しく効くことの検証と修正（17-A）
- Hierarchy 実用化: entity rename、cascade 削除の決着、選択同期（17-B）
- Game View 安定化: リサイズ・aspect・選択 entity の確認導線（17-C）
- フルループの回帰チェックリスト（17-D）

### 作らないもの

- gizmo（3D ハンドル）による直接操作
- マルチ scene 同時編集 / prefab
- 新規 component / 新規 runtime 機能

---

## Design Decisions

### cascade 削除の決着（9-0 の保留判断）

9-0 では子持ち entity の削除を `entity.has_children` 診断で拒否し、
cascade は「9-E 実装中に必要性を判断する別タスク」とした。本フェーズで
決着させる: cascade 削除（サブツリー一括 + 逆順再作成 inverse）を
`AuthoringCommand` として実装するか、UI 側で「子を先に削除してください」
誘導に留めるかを、実際の編集体験で判断して ADR 不要の範囲
（単一クレート: authoring）なら PR 説明に記録する。

### 回帰チェックリストは手順書として docs に置く

自動化できる項目は Phase 20-C のスモークテストへ送り、人間の目視が
必要な項目（描画の見た目・パネル操作感）をチェックリストに残す。

---

## Implementation Plan

### 17-A: Camera / Light / Material 編集の安定化

- scene の `engine.camera` 編集（位置・向き・fov 等）が Play に反映される
- `engine.directional_light` の方向・色・強度の編集が反映される
- camera がある scene で default camera が挿入されないこと（15-C の検証）
- material の色変更が Game View で確認できる

### 17-B: Hierarchy 実用化

- entity rename（既存 `AuthoringCommand` を UI に接続）
- 子持ち entity の削除方針の決着（上記 Design Decisions）
- Hierarchy 選択 ↔ Inspector 表示の同期が壊れるケースの修正

### 17-C: Game View 安定化

- パネルリサイズ・最小化・再生中ドラッグで描画が破綻しない
- aspect 比がレンダーターゲットサイズから正しく導出される
- 選択 entity の確認導線（最低限: debug axes 表示 or 選択 entity の
  位置情報表示。gizmo は作らない）

### 17-D: 回帰チェックリスト

「project open → scene open → entity 追加 → component 編集 → Play →
Stop → 保存 → 再起動 → 開き直し」を手順書化し、
`docs/phases/phase-17-scene-editing.md` 末尾（または別ファイル）に置く。
Phase 18〜20 の完了判定で再利用する。

---

## Cautions（注意点・落とし穴）

**「動く」と「使える」の混同**:
このフェーズの完了は機能の存在ではなくチェックリストの全項目グリーンで
判定する。

**修正の untracked 化**:
17-A〜C で見つかるバグ修正には必ず回帰テストを付ける
（RUST_CODE_STYLE §11: バグ修正はテスト必須）。

---

## Prohibited（禁止事項）

- gizmo・prefab・新規 runtime 機能の追加を禁止（スコープ外）
- チェックリスト未消化での完了宣言を禁止

---

## Completion Criteria（完了基準）

- フル編集ループの回帰チェックリストが全項目グリーン
- camera / light / material の編集結果が Play に正しく反映される
- 17-A〜C のバグ修正すべてに回帰テストが付いている
- `cargo test --workspace` が通る

---

## Feeds Into（次フェーズへの依存）

- Phase 18: 安定した編集ループの上に scene reload を載せる
- Phase 20: チェックリストが受け入れ判定の土台になる

---

## 17-D: 回帰チェックリスト

手動確認が必要な項目を列挙する。自動化できる項目は `cargo test` で固定済み。

### Project / Scene 操作

- [ ] Project Folder を開くと asset_manifest.json が自動ロードされる
- [ ] `*.scene.json` を開くと Scene Hierarchy にエンティティが表示される
- [ ] 未保存変更がある状態で別ファイルを開こうとすると Save/Discard/Cancel ダイアログが出る
- [ ] Save でファイルが更新され、再起動後に開き直すと同じ内容が復元される

### Hierarchy / Entity 編集

- [ ] `+` ボタンで新規エンティティが追加され、Inspector に name/display_name 編集欄が表示される
- [ ] name/display_name を編集すると Hierarchy の表示が即座に更新される
- [ ] Hierarchy で別エンティティを選択すると Inspector が切り替わる
- [ ] Context Menu → Delete Entity で葉エンティティが削除され、選択もクリアされる
- [ ] 子を持つエンティティに Delete を試みるとエラー診断が表示され、選択は保持される
- [ ] Undo (Ctrl+Z) でエンティティ追加・削除・コンポーネント編集が元に戻る

### Component Inspector / Add Component

- [ ] Add Component ドロップダウンに engine.camera / engine.directional_light /
  engine.ambient_light / engine.player_controller が表示される
- [ ] Add Component で追加した camera/light コンポーネントの初期値が Inspector に表示される
- [ ] Transform の x/y/z ドラッグ中はプレビュー更新され、離すと 1 undo エントリになる
- [ ] Material ピッカーで Built-in Blue / Built-in Orange を切り替えられる
- [ ] Mesh ピッカーに組み込み Triangle / Quad が表示される

### Play / Stop

- [ ] Play ボタンで Game View が起動し、scene の Transform に従ってカメラが配置される
- [ ] scene に engine.camera エンティティがあれば default camera 診断が出ない
- [ ] scene に engine.directional_light を追加して Play すると影響が目視できる
- [ ] Material を Orange に変更して Play すると Game View でオレンジ色になる
- [ ] Stop で Game View が消え、Inspector・Hierarchy は Play 前の状態に戻る
- [ ] Play 中に Game View パネルをリサイズしてもクラッシュしない
- [ ] Play 中に Game View パネルをリサイズすると次 tick でカメラ aspect が補正される

### Asset Browser

- [ ] Project を開くと meshes/ フォルダが Asset Browser に表示される
- [ ] OBJ ファイルを右クリック → Register で asset_manifest.json に追記される
- [ ] 同じファイルを再度 Register しようとすると already_registered 診断が出る
- [ ] 登録済み OBJ が Mesh ピッカーに表示される
- [ ] Play 開始時に登録 OBJ が実際にロードされ、fallback triangle より頂点数が多い
