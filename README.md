# refrain

A DAW for macOS and Windows, written in Rust.

[日本語](README.ja.md)

> Status: very early. refrain hosts CLAP instruments and plays them from a MIDI
> input, on an audio device or offline into a WAV file. There is no GUI yet,
> only the `refrain` command.

## Goals

- Host **VST3** and **CLAP** plugins, so commercial instruments and effects work.
- One codebase for macOS and Windows.
- Stay **MIT**. Every dependency is permissively licensed, and CI checks it.

## Layout

```
crates/
  refrain-engine/      the real-time engine. No OS or device code.
  refrain-clap/        hosts CLAP plugins inside the engine
  refrain-test-synth/  a small CLAP instrument, used to test hosting
  refrain/             the `refrain` command: audio devices, MIDI, WAV output
docs/
  en/                  documentation in English
  ja/                  documentation in Japanese
```

The engine runs the same way inside a device callback and in an offline
render. CI has no sound card, so it tests the engine through the offline path.

## Try it

You need [rustup](https://rustup.rs/). The toolchain version is pinned in
`rust-toolchain.toml` and is installed on first use.

```sh
cargo run -- devices                       # list audio output devices
cargo run -- midi                          # list MIDI inputs
cargo run -- plugins                       # list installed CLAP plugins
cargo run -- sine --freq 440 --seconds 2   # play a sine wave
cargo test
```

To hear a CLAP instrument, build the test synth and play it. With a MIDI
keyboard connected, play it from the keyboard; without one, `--demo` plays a
short phrase:

```sh
cargo build -p refrain-test-synth
cargo run -- play target/debug/librefrain_test_synth.dylib           # from a MIDI keyboard
cargo run -- play target/debug/librefrain_test_synth.dylib --demo    # a short phrase
cargo run -- render out.wav --plugin target/debug/librefrain_test_synth.dylib
```

On Windows the file is `target\debug\refrain_test_synth.dll`.

An installed instrument works the same way: pass its `.clap` file. Loading a
plugin runs its code inside refrain.

## Documentation

- [Architecture](docs/en/architecture.md)
- [Roadmap](docs/en/roadmap.md)
- [Licensing policy](docs/en/licensing.md): what may and may not go into this repository

## License

[MIT](LICENSE). Third-party licenses are listed in
[THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md).

VST is a registered trademark of Steinberg Media Technologies GmbH.
ASIO is a trademark of Steinberg Media Technologies GmbH.
