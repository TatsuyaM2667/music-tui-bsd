# music-tui

[![Rust](https://img.shields.io/badge/language-Rust-orange.svg)](https://www.rust-lang.org/)
[![Odin](https://img.shields.io/badge/language-Odin-blueviolet.svg)](https://odin-lang.org/)
[![Zig](https://img.shields.io/badge/language-Zig-f7a41d.svg)](https://ziglang.org/)
[![License](https://img.shields.io/badge/license-Custom%20Non--Commercial-yellow.svg)](LICENSE)
[![Platform](https://img.shields.io/badge/platform-FreeBSD%20%7C%20Linux-lightgrey.svg)](https://www.freebsd.org/)

`music-tui` は、Rust / Odin / Zig の3言語を組み合わせたターミナルベースの音楽プレイヤーです。ターミナル上で動画(MV)をレンダリングするなど、従来のTUIアプリにはない高度なマルチメディア体験を提供します。

> **Note**: FreeBSD と Linux(Chimera Linux など)で動きます。[TatsuyaM2667/music-tui](https://github.com/TatsuyaM2667/music-tui) を FreeBSD 向けに整えたものです。

## 特徴

### プレイヤー機能

- **ストリーミング再生**: クラウドストレージ(R2)からリアルタイムでストリーミング再生。
- **歌詞表示**: LRC形式のタイムスタンプ付き歌詞をパースし、再生位置に合わせてスクロール表示。
- **お気に入り機能**: 曲をお気に入りに登録し、お気に入りのみの表示に絞り込み。
- **プレイリスト**: カスタムプレイリストの作成・管理。
- **シャッフル再生**: ランダム再生対応。
- **音量調整**: キーボードから音量操作可能。
- **OSメディア統合**: MPRIS(FreeBSD / Linux の D-Bus)経由でOSのメディアコントロールに対応。

### ターミナル動画レンダリング

本プロジェクトの最大の特徴は、**ターミナル上でMVをリアルタイム再生**する機能です。2つの異なるレンダリング手法を実装しています。

#### Braille ドットアートレンダラ (Odin)

[`braille_renderer.odin`](src/braille_renderer.odin) は Odin 言語で実装されたブレイル文字レンダラーです。

- **原理**: 各ターミナルセルを2x4ドットのブレイルUnicode文字 (U+2800〜) にマッピングし、1セルあたり8ピクセル分の情報を表現。
- **色処理**: 明度加重平均 (luminance-weighted averaging) により、明るいピクセルが自然に色を支配する鮮やかな色再現を実現。さらに彩度ブースト処理でdot-art特有のビビッドな見た目を演出。
- **アスペクト比保持**: 元画像のアスペクト比を維持したリサイズと、余白のレターボックス処理。
- **用途**: 高精細な動画フレームのターミナル表示。細かいディテールを表現可能。

#### Half-Block セルレンダラ (Zig)

[`video_renderer.zig`](src/video_renderer.zig) は Zig 言語で実装されたハーフブロック文字レンダラーです。

- **原理**: 各ターミナルセルを上下2ピクセルに割り当て、前景色(上)と背景色(下)のRGB値で表現。1セルあたり2ピクセル分の情報を表示。
- **アス比保持**: Odin版と同じく、アスペクト比を維持したレターボックス処理を実装。
- **用途**: 高フレームレートが求められるシーン向けの軽量レンダラ。

### アーキテクチャ

```
┌─────────────────────────────────────────────┐
│                  Rust (TUI)                  │
│  ┌──────────┐ ┌──────────┐ ┌──────────────┐ │
│  │ ratatui  │ │  rodio   │ │    tokio     │ │
│  │  (UI)    │ │ (audio)  │ │  (async)     │ │
│  └────┬─────┘ └──────────┘ └──────────────┘ │
│       │                                      │
│       │ FFI (C ABI)                          │
│  ┌────┴─────────────────┐                    │
│  │   Odin / Zig         │                    │
│  │  ピクセル→セル変換    │                    │
│  └──────────────────────┘                    │
└─────────────────────────────────────────────┘
         │
         │ HTTP (ffmpeg process)
         ▼
┌─────────────────────┐
│  Cloudflare Workers  │
│  (R2 + API Server)   │
└─────────────────────┘
```

- **Rust**: TUIの描画、入力処理、オーディオ再生、ネットワーク通信を担当。
- **Odin / Zig**: ピクセルバッファをターミナルセルに変換するCPU集約型処理をFFI経由で担当。C ABIでRustから呼び出し。
- **build.rs**: Odinの静的ライブラリを自動コンパイルし、Rustにリンク。
- **Cloudflare Workers**: 音楽ファイルのR2ストリーミング、インデックス配信、歌詞取得などのバックエンドAPI。

## 必要条件

- **Rust** (stable)
- **Odin** (brailleレンダラのコンパイルに必要)
- **ffmpeg** (動画/音声ストリーミングのデコードに必要)
- **mpv** (外部動画再生オプション `V` に必要)
- **ALSA** (音声出力。rodio / cpal が使用)
- **D-Bus** (MPRIS。ダッシュボードなどへの曲情報・アルバムアート・再生位置)
- **ネットワーク接続**: ストリーミング再生および歌詞の取得に必要。

### FreeBSD

```sh
doas pkg install rust pkgconf alsa-lib alsa-plugins ffmpeg mpv dbus
```

- 音声は ALSA → OSS(`alsa-plugins` の OSS プラグイン)で出ます。出力先は OSS の既定(`sysctl hw.snd.default_unit`)です。
- Odin は FreeBSD のパッケージに無いため、ソースからビルドして PATH に置くか、`ODIN=/path/to/odin cargo build --release` で場所を指定してください。
  [ame-shell](https://github.com/TatsuyaM2667/ame-shell) の FreeBSD インストーラを使っている場合は、それが作った `/usr/local/ame-toolchain/odin-*/odin` を自動で使います。
- MPRIS は D-Bus のセッションバスがあれば有効です(ame-shell のセッションでは自動)。

### Linux

ディストリのパッケージで `rust` `alsa-lib`(開発用ヘッダ)`pkg-config` `ffmpeg` `mpv` と Odin を入れてください。
musl の環境(Chimera Linux / Alpine など)では、ALSA が PipeWire のプラグインを読めるよう動的リンクでビルドします(`.cargo/config.toml` で設定済み)。

## インストールと実行

1. リポジトリをクローンします。
2. `.env.example` を参考に `.env` ファイルを作成し、必要な環境変数を設定します。

```sh
cp .env.example .env
# .env を編集して WORKERS_URL を設定
```

3. ビルド＆実行:

```sh
cargo run --release
```

## ame-shell との連携

[ame-shell](https://github.com/TatsuyaM2667/ame-shell) のダッシュボード(メディア)に、アルバム名・アルバムアート・同期歌詞が出ます。

- MPRIS で曲名・アーティスト・アルバム・長さ・アルバムアート・再生位置(再生中は 1 秒ごと)を送ります。ダッシュボードからのシーク(歌詞の行をクリック)にも対応。
- 取得した同期歌詞を `$XDG_RUNTIME_DIR/lyrics/music_tui.lrc`(`[ti:]` `[ar:]` `[al:]` 付き)に書き出し、ame-shell はそれを優先して表示します。
## 操作方法

### ノーマルモード

| キー | アクション |
| :--- | :--- |
| `q` | 終了 |
| `Tab` | ペイン切り替え (メニュー ↔ コンテンツ) |
| `/` | 検索モードへ移行 |
| `Up` / `Down` | 項目の選択移動 |
| `Left` | 前の階層に戻る / 連打で5秒後シーク |
| `Right` | 次の曲へ再生 / 連打で5秒前シーク |
| `Enter` | 選択した項目を開く / 曲を再生 |
| `Space` | 再生 / 一時停止 |
| `v` | ターミナル内MV再生 (brailleレンダラ) |
| `V` | 外部mpvで動画を再生 |
| `f` | お気に入りの切り替え (現在の曲) |
| `F` | お気に入り表示の切り替え |
| `s` | シャッフル再生のON/OFF |
| `p` | 現在の曲をプレイリストに追加 |
| `+` / `-` | 音量調整 |
| `Alt+Up` / `Alt+Down` | 曲の並べ替え |

### 検索モード

- 文字入力: 検索クエリの入力 (アーティスト名・アルバム名で絞り込み可能)。
- `Backspace`: 1文字削除。
- `Esc` / `Enter`: ノーマルモードに戻る。

## プロジェクト構成

```
music-tui/
├── src/
│   ├── main.rs              # エントリポイント、イベントループ
│   ├── state.rs             # アプリケーション状態管理
│   ├── ui.rs                # TUI描画 (ratatui)
│   ├── player.rs            # オーディオ再生 (rodio)
│   ├── api.rs               # クラウドAPI通信
│   ├── renderer.rs          # Rust↔Odin FFI ブリッジ
│   ├── braille_renderer.odin # Brailleドットアートレンダラ (Odin)
│   └── video_renderer.zig   # Half-Blockセルレンダラ (Zig)
├── worker.js                # Cloudflare Workers (APIサーバー)
├── sync.js                  # R2同期スクリプト
├── build.rs                 # Odin静的ライブラリのビルドスクリプト
├── Cargo.toml
├── package.json
└── .env.example
```

## 技術スタック

| 役割 | 技術 |
| :--- | :--- |
| メイン言語 | [Rust](https://www.rust-lang.org/) |
| TUI フレームワーク | [ratatui](https://github.com/ratatui/ratatui) |
| オーディオ再生 | [rodio](https://github.com/RustAudio/rodio) |
| 非同期ランタイム | [tokio](https://github.com/tokio-rs/tokio) |
| HTTP クライアント | [reqwest](https://github.com/seanmonstar/reqwest) |
| ピクセル→セル変換 | [Odin](https://odin-lang.org/) / [Zig](https://ziglang.org/) |
| 動画デコード | [ffmpeg](https://ffmpeg.org/) (外部プロセス) |
| OSメディア統合 | [souvlaki](https://github.com/SinonoLess/souvlaki) (MPRIS) |
| バックエンド | [Cloudflare Workers](https://workers.cloudflare.com/) + R2 |

## ライセンス

[Custom Non-Commercial License](LICENSE)

> **⚠️ 重要：利用規約**
> 1. **メンションの義務**: 本プロジェクトを使用、改変、または再配布する場合、作者の GitHub アカウント (**[@TatsuyaM2667](https://github.com/TatsuyaM2667)**) を明記（メンションまたはクレジット表記）することが**必須条件**です。
> 2. **商用利用の禁止**: 本ソフトウェアおよびその派生物を商用目的で利用することは**一切許可されません**。

---

`#Rust` `#Odin` `#Zig` `#TUI` `#MusicPlayer` `#ArchLinux` `#Terminal` `#Ratatui` `#Braille`
