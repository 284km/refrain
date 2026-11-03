# refrain

macOS と Windows で動く DAW です。Rust で書いています。

[English](README.md)

> 状態: ごく初期の段階です。エンジンはオーディオデバイスでサイン波を鳴らせます。
> また、デバイスを使わずに WAV ファイルへ書き出せます。次はプラグインのホスト機能です。

## 目標

- **VST3** と **CLAP** のプラグインをホストし、市販の音源やエフェクトを使えるようにする
- macOS と Windows を 1 つのコードで動かす
- **MIT** のままにする。依存はすべて許容型のライセンスにし、CI で検査する

## 構成

```
crates/
  refrain-engine/   リアルタイムのエンジン。OS やデバイスのコードを持たない
  refrain/          `refrain` コマンド。オーディオデバイスと WAV の出力
docs/
  en/               英語のドキュメント
  ja/               日本語のドキュメント
```

エンジンは、デバイスのコールバックの中でも、デバイスを使わない書き出しでも同じように動きます。
CI のマシンにはサウンドカードが無いので、CI では書き出しの経路でエンジンを検査します。

## 試す

[rustup](https://rustup.rs/) が必要です。ツールチェーンの版は `rust-toolchain.toml` で固定しており、
初回の実行時に自動で入ります。

```sh
cargo run -- devices                       # 出力デバイスの一覧
cargo run -- sine --freq 440 --seconds 2   # サイン波を鳴らす
cargo run -- render out.wav --seconds 2    # WAV ファイルに書き出す
cargo test
```

## ドキュメント

- [設計](docs/ja/architecture.md)
- [ロードマップ](docs/ja/roadmap.md)
- [ライセンスの方針](docs/ja/licensing.md): このリポジトリに入れてよいもの、入れてはいけないもの

## ライセンス

[MIT](LICENSE) です。第三者のライセンスは [THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md) にあります。

VST は Steinberg Media Technologies GmbH の登録商標です。
ASIO は Steinberg Media Technologies GmbH の商標です。
