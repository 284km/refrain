# Third-party notices

refrain is MIT-licensed and depends only on permissively licensed crates.
`cargo deny check licenses` enforces this in CI (see [`deny.toml`](deny.toml)).

## Direct dependencies

| Crate | License | Used for |
|---|---|---|
| [cpal](https://github.com/RustAudio/cpal) | Apache-2.0 | Audio device I/O on macOS (CoreAudio) and Windows (WASAPI) |
| [hound](https://github.com/ruuda/hound) | Apache-2.0 | Writing WAV files |
| [midir](https://github.com/Boddlnagg/midir) | MIT | MIDI input |
| [rtrb](https://github.com/mgeier/rtrb) | MIT OR Apache-2.0 | Lock-free queue from the MIDI thread to the audio thread |
| [clack-host](https://github.com/prokopyl/clack) | MIT OR Apache-2.0 | Hosting CLAP plugins |
| [clack-extensions](https://github.com/prokopyl/clack) | MIT OR Apache-2.0 | CLAP extensions (audio ports, note ports, log, params) |
| [clack-plugin](https://github.com/prokopyl/clack) | MIT OR Apache-2.0 | The CLAP test plugins only, which are not part of refrain itself |
| [vst3](https://github.com/coupler-rs/vst3-rs) | MIT OR Apache-2.0 | Bindings to the VST3 API, generated from the MIT-licensed VST3 SDK headers |
| [libloading](https://github.com/nagisa/rust_libloading) | ISC | Loading VST3 modules |
| [winit](https://github.com/rust-windowing/winit) | Apache-2.0 | refrain's window, and the windows that hold plugins' editors |
| [egui](https://github.com/emilk/egui) | MIT OR Apache-2.0 | Drawing refrain's window. Default features are off, so egui's bundled fonts are not used |
| [egui-winit](https://github.com/emilk/egui) | MIT OR Apache-2.0 | Input from the window to egui |
| [egui-wgpu](https://github.com/emilk/egui) | MIT OR Apache-2.0 | Painting egui through wgpu |
| [wgpu](https://github.com/gfx-rs/wgpu) | MIT OR Apache-2.0 | The GPU, through Metal on macOS and Direct3D 12 on Windows |
| [pollster](https://github.com/zesterer/pollster) | MIT OR Apache-2.0 | Waiting for wgpu's set-up |
| [rfd](https://github.com/PolyMeilex/rfd) | MIT | The system's dialogs for opening and saving projects and picking plugin files |
| [egui_kittest](https://github.com/emilk/egui) | MIT OR Apache-2.0 | Tests only: clicking through the window without a screen |
| [raw-window-handle](https://github.com/rust-windowing/raw-window-handle) | MIT OR Apache-2.0 OR Zlib | Passing that window to plugins |
| [serde](https://github.com/serde-rs/serde) | MIT OR Apache-2.0 | Project files |
| [serde_json](https://github.com/serde-rs/json) | MIT OR Apache-2.0 | Project files are JSON |
| [base64](https://github.com/marshallpierce/rust-base64) | MIT OR Apache-2.0 | Plugin states inside project files |
| [windows-sys](https://github.com/microsoft/windows-rs) | MIT OR Apache-2.0 | Windows only: checking the editor was embedded, and the test plugins' editor |

Their transitive dependencies are listed by `cargo tree` and are all under
licenses in the allow list of `deny.toml`.

## Binary releases

There are no binary releases yet. Releases will ship a generated file with the
full license text and copyright notice of every crate built into the binary,
as the MIT and Apache-2.0 licenses require.

## Not included

This repository does not contain, and will not accept:

- the Steinberg ASIO SDK,
- the VST 2 SDK or headers derived from it,
- third-party plugin binaries, samples or presets.

See [docs/en/licensing.md](docs/en/licensing.md) for the reasons.
