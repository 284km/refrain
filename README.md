# refrain

A DAW for macOS and Windows, written in Rust.

[日本語](README.ja.md)

> Status: very early. refrain hosts CLAP and VST3 instruments and plays them
> from a MIDI input, sustain pedal and pitch bend included, on an audio device
> or offline into a WAV file, with the plugin's editor open in a window.
> refrain has no GUI of its own yet, only the `refrain` command.

## Goals

- Host **VST3** and **CLAP** plugins, so commercial instruments and effects work.
- One codebase for macOS and Windows.
- Stay **MIT**. Every dependency is permissively licensed, and CI checks it.

## Layout

```
crates/
  refrain-engine/           the real-time engine. No OS or device code.
  refrain-clap/             hosts CLAP plugins inside the engine
  refrain-vst3/             hosts VST3 plugins inside the engine
  refrain-test-tone/        the sound of the test plugins
  refrain-test-synth/       CLAP test plugins, used to test hosting
  refrain-test-synth-vst3/  VST3 test plugins that sound the same
  refrain-test-editor/      the test plugins' editor: a panel with their name
  refrain-project/          project files: which plugin, and its state
  refrain/                  the `refrain` command: audio devices, MIDI, WAV output
scripts/
  build-test-plugins.sh     builds the test plugins as .clap and .vst3 files
docs/
  en/                       documentation in English
  ja/                       documentation in Japanese
```

The engine runs the same way inside a device callback and in an offline
render. CI has no sound card, so it tests the engine through the offline path.

## Try it

You need [rustup](https://rustup.rs/). The toolchain version is pinned in
`rust-toolchain.toml` and is installed on first use.

```sh
cargo run -- devices                       # list audio output devices
cargo run -- midi                          # list MIDI inputs
cargo run -- plugins                       # list installed CLAP and VST3 plugins
cargo run -- sine --freq 440 --seconds 2   # play a sine wave
cargo test
```

To hear an instrument, build the test plugins and play one. With a MIDI
keyboard connected, play it from the keyboard; without one, `--demo` plays a
short phrase:

```sh
scripts/build-test-plugins.sh
cargo run -- play target/test-plugins/refrain-test-synth.clap          # from a MIDI keyboard
cargo run -- play target/test-plugins/refrain-test-synth.vst3 --demo   # a short phrase
cargo run -- render out.wav --plugin target/test-plugins/refrain-test-synth.vst3
cargo run -- editor target/test-plugins/refrain-test-synth.clap        # the editor alone
```

`play` opens the plugin's editor in a window when it has one; `--no-editor`
leaves it closed.

To keep a sound, save the plugin and its state as a project when you close
the editor, and open the project instead of the plugin next time:

```sh
cargo run -- play target/test-plugins/refrain-test-synth.vst3 --save song.refrain
cargo run -- play song.refrain                        # the same plugin, the same sound
cargo run -- render out.wav --plugin song.refrain
```

A project is a small JSON file. A plugin path in it that is relative is
relative to the project's folder.

An installed instrument works the same way: pass its `.clap` or `.vst3`
file or bundle. When a file holds several plugins, `--id` chooses one
(for VST3, by name or class ID). Loading a plugin runs its code inside refrain.

## Documentation

- [Architecture](docs/en/architecture.md)
- [Roadmap](docs/en/roadmap.md)
- [Licensing policy](docs/en/licensing.md): what may and may not go into this repository

## License

[MIT](LICENSE). Third-party licenses are listed in
[THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md).

VST is a registered trademark of Steinberg Media Technologies GmbH.
ASIO is a trademark of Steinberg Media Technologies GmbH.
