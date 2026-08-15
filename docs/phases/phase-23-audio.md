# Phase 23: Audio — BGM / SE 再生

> **2026-06-13 再構成**: 旧 Phase 19。rodio 依存はこのフェーズで追加する。
> 旧→新対応表は `docs/IMPLEMENTATION_PLAN_PHASE9_ONWARDS.md` を参照。

## Goal

BGM のループ再生とワンショット効果音（SE）をゲームから鳴らせるようにする。  
衝突・スコア獲得などのゲームイベントに SE を紐付ける基盤を作る。

---

## Why（なぜこのフェーズが必要か・なぜこの順序か）

**Phase 18 の後にこのフェーズが来る理由**:  
Phase 19 は他のゲームシステムに依存しない独立したサブシステム。  
Phase 20 のサンプルゲームで「衝突 SE」「BGM」を使うために、事前に実装する。  
Phase 18 より後であれば任意のタイミングで実装できる。

**なぜ `rodio` クレートを使うか**:  
pure Rust の音声再生ライブラリで、Windows / Mac / Linux に対応。  
`cpal` の上に構築されており、クロスプラットフォームで安定している。  
WASM には未対応だが、Phase 19 では desktop のみを対象とする。  
`bevy_audio` は Bevy の ECS と密結合しており流用が難しい。

**なぜ `AudioSystem` を ECS resource にするか**:  
`rodio::Sink` はスレッドセーフで `Send + Sync` を実装している。  
resource にすることで `Res<AudioSystem>` として任意のシステムから音声を制御できる。

---

## Scope

### 作るもの

- `AudioSystem` resource — `rodio` を内部に持つ音声マネージャ
- `AudioAsset` — WAV / OGG ファイルをロードしてメモリに保持
- `play_se(handle)` — ワンショット効果音を鳴らす
- `play_bgm(handle)` — ループ BGM を再生する
- `stop_bgm()` — BGM を停止する
- `set_master_volume(f32)` — マスター音量の制御
- `AssetServer::load_audio(path)` — 音声ファイルを Phase 14 のパターンで読み込む

### 作らないもの

- 3D 空間音響（positional audio）
- Reverb / EQ などのエフェクト
- WASM での音声再生
- 複数 BGM のクロスフェード
- ピッチ変更

---

## Design Decisions

### なぜ `rodio::Sink` を BGM に使うか

`Sink` は再生中の音声ストリームを制御できる（`pause()` / `play()` / `stop()`）。  
ループ再生は `rodio::source::Repeat` でラップすることで実現する。  
SE はワンショットなので `OutputStream::play_raw()` で直接再生する。

### なぜ Audio を ECS system から分離しないか

当初「AudioSystem を別スレッドで動かす」案もあったが、  
`rodio` は `OutputStream` を保持したスレッドが生きている間のみ音声が鳴る。  
ゲームのメインスレッドに `OutputStream` を保持することで、スレッド問題を回避する。

### WAV vs OGG の優先度

WAV: 無圧縮のため CPU コストが低いが、ファイルサイズが大きい。SE に向く。  
OGG: 圧縮されてファイルサイズが小さいが、デコードに CPU を使う。BGM に向く。  
`rodio` は両方をサポートしており、拡張子で自動判別できる。

### なぜ `AssetServer::load_audio` で事前ロードするか

ゲーム中に突然ファイルを読むと、IO でフレームが止まることがある（ヒッチ）。  
game 開始前（Play ボタン押下直後）にまとめてロードしてキャッシュしておく。  
`play_se` はキャッシュ済みの `AudioAsset` を使って即座に再生する。

---

## Implementation Plan

### 19-A: 依存クレートの追加

```toml
# crates/engine/Cargo.toml
[dependencies]
rodio = { version = "0.20", default-features = false, features = ["wav", "vorbis"] }
```

`default-features = false` で不要な codec を除外してコンパイルを速くする。

### 19-B: AudioAsset と AudioSystem

```rust
// crates/engine/src/audio.rs（新規）
pub struct AudioAsset {
    pub data: Arc<Vec<u8>>,   // ファイル内容をメモリに保持
    pub sample_rate: u32,
}

pub struct AudioSystem {
    _stream: rodio::OutputStream,           // Drop されると音が止まる（保持用）
    stream_handle: rodio::OutputStreamHandle,
    bgm_sink: Option<rodio::Sink>,
    master_volume: f32,
}

impl AudioSystem {
    pub fn new() -> Result<Self, AudioError> {
        let (_stream, stream_handle) = rodio::OutputStream::try_default()
            .map_err(AudioError::OutputStream)?;
        Ok(Self {
            _stream,
            stream_handle,
            bgm_sink: None,
            master_volume: 1.0,
        })
    }

    pub fn play_se(&self, asset: &AudioAsset) {
        let cursor = std::io::Cursor::new(Arc::clone(&asset.data));
        let source = rodio::Decoder::new(cursor).unwrap();
        self.stream_handle.play_raw(source.convert_samples()).ok();
    }

    pub fn play_bgm(&mut self, asset: &AudioAsset) {
        self.stop_bgm();
        let sink = rodio::Sink::try_new(&self.stream_handle).unwrap();
        sink.set_volume(self.master_volume);
        let cursor = std::io::Cursor::new(Arc::clone(&asset.data));
        let source = rodio::Decoder::new(cursor).unwrap().repeat_infinite();
        sink.append(source);
        self.bgm_sink = Some(sink);
    }

    pub fn stop_bgm(&mut self) {
        if let Some(sink) = self.bgm_sink.take() {
            sink.stop();
        }
    }

    pub fn set_master_volume(&mut self, volume: f32) {
        self.master_volume = volume.clamp(0.0, 1.0);
        if let Some(sink) = &self.bgm_sink {
            sink.set_volume(self.master_volume);
        }
    }
}
```

### 19-C: AssetServer への音声ロード追加

```rust
// crates/engine/src/asset.rs に追加
pub struct AssetServer {
    // 既存フィールド...
    audio_cache: HashMap<String, Handle<AudioAsset>>,   // 追加
}

impl AssetServer {
    pub fn load_audio(
        &mut self,
        relative_path: &str,
        assets: &mut Assets<AudioAsset>,
    ) -> Result<Handle<AudioAsset>, AssetLoadError> {
        if let Some(handle) = self.audio_cache.get(relative_path) {
            return Ok(*handle);
        }
        let path = self.resolve(relative_path)?;
        let data = std::fs::read(&path).map_err(AssetLoadError::Io)?;
        let audio = AudioAsset { data: Arc::new(data), sample_rate: 44100 };
        let handle = assets.add(audio);
        self.audio_cache.insert(relative_path.to_string(), handle);
        Ok(handle)
    }
}
```

### 19-D: Play 開始時の AudioSystem 初期化

```rust
// EditorApp の Play フロー（Phase 10 参照）に追加
match AudioSystem::new() {
    Ok(audio_system) => world.insert_resource(audio_system),
    Err(e) => {
        // 音声なしで続行（ヘッドレス環境等）
        diagnostics.push(Diagnostic::warning("audio.no_output_device", e.to_string()));
    }
}
```

Stop 時に `AudioSystem` が Drop → `Sink::stop()` が呼ばれて BGM が止まる。

### 19-E: ゲームシステムからの使い方（サンプル）

```rust
// スコア獲得時に SE を鳴らす system
pub fn score_sound_system(
    mut events: EventReader<ScoreEvent>,
    audio: Res<AudioSystem>,
    score_se: Res<ScoreSoundHandle>,  // Handle<AudioAsset>
    audio_assets: Res<Assets<AudioAsset>>,
) {
    for _ in events.read() {
        if let Some(asset) = audio_assets.get(score_se.0) {
            audio.play_se(asset);
        }
    }
}
```

---

## Cautions（注意点・落とし穴）

**`rodio::OutputStream` を保持し続ける**:  
`_stream` フィールドが Drop されると即座に音声が止まる。  
`AudioSystem` の `_stream` フィールド名をアンダースコアで始めているのは「意図的に保持している」シグナル。  
この変数を削除・リネーム・早期 Drop しないこと。

**ヘッドレス環境（CI、テスト）での音声デバイスなし**:  
`rodio::OutputStream::try_default()` がエラーになる環境（CI サーバー等）でクラッシュしない。  
`Result<AudioSystem, AudioError>` として扱い、音声なしで続行できる設計にする。

**WASM では未対応**:  
`rodio` は WASM をサポートしていない。  
WASM ビルド時は `AudioSystem` を空の stub に差し替えるか、feature flag で無効化する。

**大きな WAV ファイルをメモリに乗せる**:  
BGM を WAV で持つとメモリ消費が大きい（1 分で ~10MB）。  
BGM には OGG（圧縮）を使うことを推奨する。ドキュメントに記載する。

---

## Prohibited（禁止事項）

- 3D 空間音響をこのフェーズで実装することを禁止
- WASM での音声再生をこのフェーズで実装することを禁止
- `AudioSystem::play_se` の中でファイルを読むことを禁止（事前ロードのみ）

---

## Completion Criteria（完了基準）

- `audio.play_bgm(&bgm_asset)` でループ BGM が再生される
- `audio.play_se(&se_asset)` でワンショット SE が鳴る
- Play → Stop で BGM が停止する
- ヘッドレス環境で `AudioSystem::new()` がエラーを返してもゲームが続行する
- `cargo test --workspace` が通る（音声デバイスなしの環境でも）

---

## Feeds Into（次フェーズへの依存）

- Phase 20: sample game で BGM とスコア獲得 SE を組み込む
