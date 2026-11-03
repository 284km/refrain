# Roadmap

Features land one step at a time. Each step ends with something that can be
heard or checked on both macOS and Windows.

| # | Step | State |
|---|---|---|
| 1 | Play a sine wave on a device, and render it offline | Done. Device checked on macOS; Windows is built and tested in CI |
| 2 | Load a CLAP or VST3 instrument and play it from a MIDI keyboard | Next |
| 3 | Open a plugin's editor in a window | |
| 4 | Save and restore plugin state with a project | |
| 5 | Record and play back MIDI clips on a timeline | |
| 6 | Effect chains and a mixer | |
| 7 | Render a project to a WAV file | |

## Alongside the steps

- **Low latency on Windows.** WASAPI exclusive mode through the `wasapi`
  crate, since `cpal` only uses shared mode.
- **ASIO as an opt-in build feature.** Whoever builds it supplies the ASIO
  SDK themselves. It never enters this repository.
- **Audio Units** on macOS, after VST3 and CLAP.
- **Allocation checks on the audio thread** in debug builds.
- **Test plugins built in this repository**, so CI can test plugin hosting
  without third-party binaries.
- **Release work**: generated third-party notices, macOS signing and
  notarization, Windows signing.
