# refrain

macOS と Windows で動く DAW です。Rust で書いています。

[English](README.md)

> 状態: ごく初期の段階です。CLAP のインストゥルメントを読み込み、MIDI 入力から鳴らせます。
> 音はオーディオデバイスにも、デバイスを使わずに WAV ファイルにも出せます。
> GUI はまだ無く、`refrain` コマンドだけです。

## 目標

- **VST3** と **CLAP** のプラグインをホストし、市販の音源やエフェクトを使えるようにする
- macOS と Windows を 1 つのコードで動かす
- **MIT** のままにする。依存はすべて許容型のライセンスにし、CI で検査する

## 構成

```
crates/
  refrain-engine/      リアルタイムのエンジン。OS やデバイスのコードを持たない
  refrain-clap/        CLAP プラグインをエンジンの中でホストする
  refrain-test-synth/  ホスト機能のテストに使う小さな CLAP インストゥルメント
  refrain/             `refrain` コマンド。オーディオデバイス、MIDI、WAV の出力
docs/
  en/                  英語のドキュメント
  ja/                  日本語のドキュメント
```

エンジンは、デバイスのコールバックの中でも、デバイスを使わない書き出しでも同じように動きます。
CI のマシンにはサウンドカードが無いので、CI では書き出しの経路でエンジンを検査します。

## 試す

[rustup](https://rustup.rs/) が必要です。ツールチェーンの版は `rust-toolchain.toml` で固定しており、
初回の実行時に自動で入ります。

```sh
cargo run -- devices                       # オーディオ出力デバイスの一覧
cargo run -- midi                          # MIDI 入力の一覧
cargo run -- plugins                       # インストールされた CLAP プラグインの一覧
cargo run -- sine --freq 440 --seconds 2   # サイン波を鳴らす
cargo test
```

CLAP のインストゥルメントを聞くには、テスト用シンセをビルドして鳴らします。MIDI キーボードが
つながっていればキーボードから弾けます。つながっていなければ、`--demo` で短いフレーズが鳴ります。

```sh
cargo build -p refrain-test-synth
cargo run -- play target/debug/librefrain_test_synth.dylib           # MIDI キーボードから
cargo run -- play target/debug/librefrain_test_synth.dylib --demo    # 短いフレーズ
cargo run -- render out.wav --plugin target/debug/librefrain_test_synth.dylib
```

Windows ではファイルが `target\debug\refrain_test_synth.dll` になります。

インストール済みのインストゥルメントも同じように、`.clap` ファイルを渡せば使えます。
プラグインを読み込むと、そのコードが refrain の中で動きます。

## ドキュメント

- [設計](docs/ja/architecture.md)
- [ロードマップ](docs/ja/roadmap.md)
- [ライセンスの方針](docs/ja/licensing.md): このリポジトリに入れてよいもの、入れてはいけないもの

## ライセンス

[MIT](LICENSE) です。第三者のライセンスは [THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md) にあります。

VST は Steinberg Media Technologies GmbH の登録商標です。
ASIO は Steinberg Media Technologies GmbH の商標です。
