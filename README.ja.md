# refrain

macOS と Windows で動く DAW です。Rust で書いています。

[English](README.md)

> 状態: ごく初期の段階です。CLAP と VST3 のインストゥルメントを読み込み、MIDI 入力から
> 鳴らせます。サステインペダルとピッチベンドも届きます。音はオーディオデバイスにも、
> デバイスを使わずに WAV ファイルにも出せます。プラグインの画面はウィンドウで開けます。
> refrain 自身の GUI はまだ無く、`refrain` コマンドだけです。

## 目標

- **VST3** と **CLAP** のプラグインをホストし、市販の音源やエフェクトを使えるようにする
- macOS と Windows を 1 つのコードで動かす
- **MIT** のままにする。依存はすべて許容型のライセンスにし、CI で検査する

## 構成

```
crates/
  refrain-engine/           リアルタイムのエンジン。OS やデバイスのコードを持たない
  refrain-clap/             CLAP プラグインをエンジンの中でホストする
  refrain-vst3/             VST3 プラグインをエンジンの中でホストする
  refrain-test-tone/        テスト用プラグインの音
  refrain-test-synth/       ホスト機能のテストに使う CLAP のプラグイン
  refrain-test-synth-vst3/  同じ音の VST3 のプラグイン
  refrain-test-editor/      テスト用プラグインの画面。名前を出すだけのパネル
  refrain-project/          プロジェクトファイル。使うプラグインとその状態
  refrain/                  `refrain` コマンド。オーディオデバイス、MIDI、WAV の出力
scripts/
  build-test-plugins.sh     テスト用プラグインを .clap と .vst3 のファイルにする
docs/
  en/                       英語のドキュメント
  ja/                       日本語のドキュメント
```

エンジンは、デバイスのコールバックの中でも、デバイスを使わない書き出しでも同じように動きます。
CI のマシンにはサウンドカードが無いので、CI では書き出しの経路でエンジンを検査します。

## 試す

[rustup](https://rustup.rs/) が必要です。ツールチェーンの版は `rust-toolchain.toml` で固定しており、
初回の実行時に自動で入ります。

```sh
cargo run -- devices                       # オーディオ出力デバイスの一覧
cargo run -- midi                          # MIDI 入力の一覧
cargo run -- plugins                       # インストールされた CLAP と VST3 のプラグインの一覧
cargo run -- sine --freq 440 --seconds 2   # サイン波を鳴らす
cargo test
```

インストゥルメントを聞くには、テスト用プラグインをビルドして鳴らします。MIDI キーボードが
つながっていればキーボードから弾けます。つながっていなければ、`--demo` で短いフレーズが鳴ります。

```sh
scripts/build-test-plugins.sh
cargo run -- play target/test-plugins/refrain-test-synth.clap          # MIDI キーボードから
cargo run -- play target/test-plugins/refrain-test-synth.vst3 --demo   # 短いフレーズ
cargo run -- render out.wav --plugin target/test-plugins/refrain-test-synth.vst3
cargo run -- editor target/test-plugins/refrain-test-synth.clap        # 画面だけを開く
```

`play` は、プラグインに画面があればウィンドウで開きます。`--no-editor` を付けると開きません。

音色を残しておくには、画面を閉じるときに、プラグインとその状態をプロジェクトとして保存します。
次からは、プラグインの代わりにプロジェクトを開きます。

```sh
cargo run -- play target/test-plugins/refrain-test-synth.vst3 --save song.refrain
cargo run -- play song.refrain                        # 同じプラグイン、同じ音
cargo run -- render out.wav --plugin song.refrain
```

プロジェクトは小さな JSON のファイルです。中のプラグインのパスが相対パスなら、プロジェクトの
フォルダからの相対パスです。

録音するには、プロジェクトに向かって弾きます。MIDI から入ったものが新しいクリップになります。
その間、すでにあるクリップも一緒に鳴るので、パートを重ねていけます。録音中はメトロノームが鳴ります。
`--no-click` で止められます。

```sh
cargo run -- record song.refrain --tempo 100          # Enter で止めると、クリップが保存される
cargo run -- play song.refrain                        # クリップが鳴り、一緒に弾ける
cargo run -- render out.wav --plugin song.refrain     # クリップを WAV ファイルへ
```

インストール済みのインストゥルメントも同じように、`.clap` か `.vst3` のファイル（またはバンドル）を
渡せば使えます。1 つのファイルに複数のプラグインがあるときは `--id` で選びます（VST3 は名前か
クラス ID）。プラグインを読み込むと、そのコードが refrain の中で動きます。

## ドキュメント

- [設計](docs/ja/architecture.md)
- [ロードマップ](docs/ja/roadmap.md)
- [ライセンスの方針](docs/ja/licensing.md): このリポジトリに入れてよいもの、入れてはいけないもの

## ライセンス

[MIT](LICENSE) です。第三者のライセンスは [THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md) にあります。

VST は Steinberg Media Technologies GmbH の登録商標です。
ASIO は Steinberg Media Technologies GmbH の商標です。
