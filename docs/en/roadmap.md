# Roadmap

Features land one step at a time. Each step ends with something that can be
heard or checked on both macOS and Windows.

| # | Step | State |
|---|---|---|
| 1 | Play a sine wave on a device, and render it offline | Done |
| 2 | Load a CLAP or VST3 instrument and play it from a MIDI keyboard | Done. Checked on a macOS device and through a virtual MIDI port; not yet with a hardware keyboard or on a Windows device |
| 2a | Sustain pedal, pitch bend and other controllers | Done. Plugins that take notes only get the pedal played for them |
| 2b | VST3 instruments | Done. Test plugins agree with the CLAP ones sample for sample; a commercial VST3 instrument loads, renders and plays on a macOS device |
| 3 | Open a plugin's editor in a window | Done. CLAP and VST3; the test plugins' editors open in CI on macOS and Windows, and a commercial VST3 editor opens on macOS |
| 4 | Save and restore plugin state with a project | Done. CLAP and VST3; test projects restore and resave in CI on macOS and Windows, and a commercial VST3 instrument's state saves and restores on macOS |
| 5 | Record and play back MIDI clips on a timeline | Done. Recording layers a new clip over the ones there, with a metronome; CI checks clips play back exactly like the same phrase sent as events |
| 6 | Effect chains and a mixer | Done. Tracks with an instrument, a chain of effects, volume, pan, mute and solo, mixed through a master chain; test effects in both formats; CI puts a project together with the project commands and checks its mix in both formats |
| 7 | Render a project to a WAV file | Done. Renders until the sound dies away, with plugins told it is offline, to 32-bit float or 16- or 24-bit integers with optional dither; CI checks the length, the file sizes and that dither is the same every time |
| 8 | refrain's own GUI: transport, tracks, clips and the mixer | Next |

## Alongside the steps

- **Note timing.** Place notes from a MIDI input at the frame they were
  played, instead of at the start of the next block. Recording already keeps
  the moment each event arrived.
- **Recording latency.** Line recorded events up with what was heard, by the
  output latency; add a count-in, clips that start later in the timeline,
  looping, and tempo changes.
- **Latency compensation.** Delay the other tracks by what an effect reports,
  so a chain with latency stays in time.
- **More of the mix**: sends, buses and groups, automation, and changing the
  mix while playing.
- **More from rendering**: single tracks (stems), sample rate conversion,
  normalizing, and other formats such as FLAC.
- **Low latency on Windows.** WASAPI exclusive mode through the `wasapi`
  crate, since `cpal` only uses shared mode.
- **ASIO as an opt-in build feature.** Whoever builds it supplies the ASIO
  SDK themselves. It never enters this repository.
- **Audio Units** on macOS, after VST3 and CLAP.
- **Allocation checks on the audio thread** in debug builds.
- **Plugin logs off the audio thread**, through a queue.
- **Scanning plugins in a separate process**, so a plugin that crashes while
  being listed does not take refrain down.
- **Release work**: generated third-party notices, macOS signing and
  notarization, Windows signing.
