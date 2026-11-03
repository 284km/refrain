# refrain

macOS と Windows で動く DAW です。Rust で書いています。

[English](README.md)

> 状態: ごく初期の段階です。CLAP と VST3 のインストゥルメントとエフェクトを読み込み、
> MIDI 入力から鳴らせます。音はオーディオデバイスにも、デバイスを使わずに WAV ファイルにも
> 出せます。MIDI のクリップを録音でき、複数のトラック（それぞれインストゥルメントと
> エフェクトの列）をミックスして、マスターのエフェクトに通せます。プラグインの画面は
> ウィンドウで開けます。`refrain gui` で、トランスポート、トラック、ミックスのある refrain 自身の
> ウィンドウが開きます。

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
  refrain-test-synth/       ホスト機能のテストに使う CLAP のプラグイン。
                            インストゥルメント 2 つとエフェクト 1 つ
  refrain-test-synth-vst3/  同じ音の VST3 のプラグイン
  refrain-test-editor/      テスト用プラグインの画面。名前を出すだけのパネル
  refrain-project/          プロジェクトファイル。トラック、プラグインとその状態、
                            ミックス、クリップ
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

1 つのプロジェクトに、複数のトラックを持てます。トラックごとに、インストゥルメント、その後ろに
並ぶエフェクト、音量、パン、ミュートとソロがあります。ミックスしたトラックは、マスターの
エフェクトを通ります。次のコマンドは、音を出さずにプロジェクトを書き換えます。

```sh
p=target/test-plugins
cargo run -- new band.refrain
cargo run -- track add band.refrain $p/refrain-test-synth.vst3 --id "refrain test synth"
cargo run -- track add band.refrain song.refrain          # song.refrain のトラックをクリップごと
cargo run -- fx add band.refrain $p/refrain-test-synth.vst3 --id "refrain test effect" --track 2
cargo run -- mix band.refrain --track 2 --volume -6 --pan -0.5
cargo run -- fx add band.refrain $p/refrain-test-synth.clap --id dev.refrain.test-effect --master
cargo run -- show band.refrain
cargo run -- record band.refrain --track 1                # MIDI はトラック 1 へ
cargo run -- editor band.refrain --track 2 --fx 1 --save band.refrain   # エフェクトの画面
```

`render` はプロジェクトを WAV ファイルに書き出します。最後のノートのあとも、音が消えるまで書き続け、
最後の無音を切り落とします。`--seconds` を付けると、その長さで書きます。書き出しのときは、
リアルタイムではないことをプラグインに伝えるので、プラグインはいちばん良い品質で処理できます。
ファイルは `--bits` で 16 か 24 を指定しない限り 32 bit の浮動小数で、`--dither` を付けると
そこに三角分布のディザーを加えます。

```sh
cargo run -- render band.wav --plugin band.refrain --bits 24
cargo run -- render band-cd.wav --plugin band.refrain --bits 16 --dither --rate 44100
```

インストール済みのインストゥルメントやエフェクトも同じように、`.clap` か `.vst3` のファイル（またはバンドル）を
渡せば使えます。1 つのファイルに複数のプラグインがあるときは `--id` で選びます（VST3 は名前か
クラス ID）。プラグインを読み込むと、そのコードが refrain の中で動きます。

## refrain のウィンドウ

`refrain gui` は、プロジェクトを refrain 自身のウィンドウで開きます。トランスポート、トラックごとの
ミックスとクリップの行、マスターがあります。再生と停止、タイムラインを押して再生位置を移すこと、
MIDI の印を付けたトラックへの録音ができます。音量、パン、ミュート、ソロは、動かすとすぐ音に
反映されます。プラグインのボタンを押すと、その画面が開きます。Save でプロジェクトに書き戻します。

```sh
cargo run -- gui band.refrain
cargo run -- gui target/test-plugins/refrain-test-synth.clap --save new.refrain
```

**Add track** と **+ effect** を押すと、プラグインを選ぶ画面が開きます。インストール先のフォルダにある
プラグインのファイルと、そのプロジェクトですでに使っているファイルが並びます。パスを打ち込んだり、
OS のファイル選択の画面で選んだりもできます。1 つのファイルに複数のプラグインがあるときは、その中から
選びます。トラック名やエフェクトを右クリックすると、削除できます。**New**、**Open…**、**Save as…** は
その名前のとおりに動きます。保存していない変更があると、New と Open は 2 回押すまで実行しません。

```sh
cargo run -- gui                                    # 新しいプロジェクト
```

クリップは、再生中もタイムラインで編集できます。押すと選択、ドラッグで拍単位の移動、右クリックで
複製と削除です（⌘D と Delete でもできます。Windows では Ctrl）。クリップをダブルクリックすると、
ピアノロールでノートを編集できます。押すとノートを足し、ドラッグで移動、右端のドラッグで長さを変え、
右クリックで消します。Undo と Redo（⌘Z と ⇧⌘Z）で、クリップとノートの編集を取り消せます。

## ドキュメント

- [設計](docs/ja/architecture.md)
- [ロードマップ](docs/ja/roadmap.md)
- [ライセンスの方針](docs/ja/licensing.md): このリポジトリに入れてよいもの、入れてはいけないもの

## ライセンス

[MIT](LICENSE) です。第三者のライセンスは [THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md) にあります。

VST は Steinberg Media Technologies GmbH の登録商標です。
ASIO は Steinberg Media Technologies GmbH の商標です。
