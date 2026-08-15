# クイックガイド: アニメーションを再生する

クリップ（キーフレーム）はエンジン内では作らない。Blender 等の DCC ツールで作り、
glTF 2.0 として持ち込む。概念の対応は次の通り。

```
DCC (Blender / Mixamo など)
  └─ モデル + スケルトン + アニメーションを作成 → glTF/.glb か FBX で export
       （FBX は ufbx 経由で直接読める。ADR 0081 / docs/FBX_IMPORT.md 参照。
        変換なしで登録できない特殊機能を使う場合のみ glTF へ変換する）
エディタ
  └─ Register Asset → Mesh / Skin / AnimationClip がサブアセット化される
entity
  ├─ Skinned Mesh Source … どの glTF/FBX ソースの mesh/skin を使うか
  └─ Animation Controller … skeleton + Animation Set + graph + 再生設定
```

## 手順

1. glTF/GLB または FBX を Asset Browser に入れて **Register Asset**
   （選択するとサブアセット一覧に `[clip]` 行が見える。Mixamo からの FBX も
   変換せずそのまま登録できる）
2. entity に **Skinned Mesh Source** を追加し、ソースを割り当てる
3. **Animation Controller** を追加する:
   - **Skeleton**: import された Skin サブアセットを選ぶ
   - **Animation Set**: Graph の Motion Slot と Animation Clip サブアセットの対応表を選ぶ
   - **Animation Set** と **Animation Graph** は再生時に両方設定する
   - Speed / Event / Root Motion を設定。Loop は旧 Graph の互換用フォールバック
4. Scene View の **Animation Preview...**、または **View → Animation Preview...**
   を開く。Clip では単体再生、Transition では From／To と開始時刻／Fade、Graph
   では実際の Parameter 遷移を Play せずに確認できる。Transition は Repeat と
   Cycle で同じブレンドを繰り返せる
5. 状態遷移（idle→walk→run など）が必要なら **File → New Animation Graph**、
   または Asset Browser の空白／フォルダを右クリックして **Create → Animation Graph**
   でアセットを作る。Asset Browserから作る場合は現在のフォルダに保存される
6. Graph Canvas の **Add Node → State** で状態を追加し、State の Inspector へ
   Motion Slot と **Playback Mode（Loop / Once）** を設定する。内部参照には自動生成された
   安定 ID が使われる
7. 右Inspectorの **Animation Parameters** で、遷移に使うパラメーターを追加する。
   種類は `Bool`、`Float`、`Trigger`。例えば `grounded: Bool`、`speed: Float`、
   `attack: Trigger` を宣言する
8. Entry または State を右クリックして **Connect From**、続いて遷移先 State を
   クリックする。State間の遷移線をクリックし、Inspectorで宣言済みParameterを選ぶ:
   - Bool: `true` または `false` を選ぶ
   - Float: `<`、`<=`、`>`、`>=`、`==`、`!=` と閾値を選ぶ
   - Trigger: Parameterを選ぶだけ
   - Parameterを使わない場合は **Unconditional** を選ぶ
   Fadeは同じInspectorでController既定値または個別秒数を設定する
9. Asset Browser で作成した Animation Graph を右クリックし、**Create Animation Set**
   を選ぶ。`*.animset.json` の各 Motion Slot に、任意の glTF／GLB／FBX 由来の
   Animation Clip サブアセット ID を割り当てる。同じ Set 内でソースが異なってよい
10. 作成した Graph と Animation Set を同じ Animation Controller に割り当てる。
    Graph 未指定時の単体クリップ再生はサポートしない

Project Rustからは`Commands::set_animation_bool`、`set_animation_float`、
`trigger_animation`で値を送る。コードで渡す名前はGraphで宣言した名前と一致させる。
Triggerは条件に一致した遷移が実際に選ばれた時だけ消費される。

```rust
commands.set_animation_bool(entity, "grounded", is_grounded);
commands.set_animation_float(entity, "speed", current_speed);
commands.trigger_animation(entity, "attack");
```

旧Graphの自由入力条件が残っている場合、InspectorはRemoved legacy dataとして表示する。
その遷移は宣言済みParameterまたはUnconditionalを選び直して置き換える。新規作成では
条件文字列を直接入力しない。

## うまくいかないとき

- 何も動かない → Animation Controller の Animation Set と Graph の両方が設定され、Graph の全 Motion Slot が Set で束縛されているか確認する
- 遷移しない → GraphでParameterが宣言され、ゲームコードから同じ名前・同じ型で値を送っているか確認する
- Parameterが一覧にない → Animation Graph右InspectorのAnimation Parametersで追加する
- 旧条件と表示される → 遷移線を選び、宣言済みParameterかUnconditionalへ設定し直す
- ポーズが崩れる → DCC 側 export で skin weight / inverse bind pose を含めたか
- クリップ一覧が空 → Reimport を実行して sub-asset を再カタログ化する
