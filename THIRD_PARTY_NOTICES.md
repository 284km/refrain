# Third-party notices

refrain is MIT-licensed and depends only on permissively licensed crates.
`cargo deny check licenses` enforces this in CI (see [`deny.toml`](deny.toml)).

## Direct dependencies

| Crate | License | Used for |
|---|---|---|
| [cpal](https://github.com/RustAudio/cpal) | Apache-2.0 | Audio device I/O on macOS (CoreAudio) and Windows (WASAPI) |
| [hound](https://github.com/ruuda/hound) | Apache-2.0 | Writing WAV files |

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
