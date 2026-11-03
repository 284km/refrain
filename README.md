# refrain

A DAW for macOS and Windows, written in Rust.

[日本語](README.ja.md)

> Status: very early. The engine plays a sine wave on an audio device and
> renders it offline to a WAV file. Plugin hosting is next.

## Goals

- Host **VST3** and **CLAP** plugins, so commercial instruments and effects work.
- One codebase for macOS and Windows.
- Stay **MIT**. Every dependency is permissively licensed, and CI checks it.

## Layout

```
crates/
  refrain-engine/   the real-time engine. No OS or device code.
  refrain/          the `refrain` command: audio devices, WAV output
docs/
  en/               documentation in English
  ja/               documentation in Japanese
```

The engine runs the same way inside a device callback and in an offline
render. CI has no sound card, so it tests the engine through the offline path.

## Try it

You need [rustup](https://rustup.rs/). The toolchain version is pinned in
`rust-toolchain.toml` and is installed on first use.

```sh
cargo run -- devices                       # list output devices
cargo run -- sine --freq 440 --seconds 2   # play a sine wave
cargo run -- render out.wav --seconds 2    # write one to a WAV file
cargo test
```

## Documentation

- [Architecture](docs/en/architecture.md)
- [Roadmap](docs/en/roadmap.md)
- [Licensing policy](docs/en/licensing.md): what may and may not go into this repository

## License

[MIT](LICENSE). Third-party licenses are listed in
[THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md).

VST is a registered trademark of Steinberg Media Technologies GmbH.
ASIO is a trademark of Steinberg Media Technologies GmbH.
