# Architecture

This page describes how refrain is put together today and the rules it keeps.
It grows as features land.

## Threads and their rules

```
MIDI thread ──(lock-free queue)──┐
                                 ▼
main thread                    audio thread
  loads and activates plugins    reads notes from the queue
  answers plugin callbacks       runs the engine and the plugin's process()
  may allocate, lock, block      must not allocate, lock, block or do I/O
```

The audio thread has a deadline. A 128-frame block at 48 kHz must be finished
in about 2.7 ms, every time. Missing it is an audible click. So code that runs
there:

- does not allocate or free memory,
- does not take locks that another thread can hold,
- does not do I/O, print, or wait.

Everything the audio thread uses is allocated before the stream starts: block
buffers, the event list for a block, and a plugin's channel buffers.

## Crates

### `refrain-engine`

The engine. It has no OS, device or file code, and no dependencies.

- `Processor` is anything that fills an interleaved `f32` buffer:
  `prepare(sample_rate, max_block_frames)`, then
  `process(out, channels, events)` again and again.
- Blocks are at most `MAX_BLOCK_FRAMES` (1024) frames and carry at most
  `MAX_BLOCK_EVENTS` (256) events, so buffers can be sized up front.
- `Event` is what a player does: a note on or off, a controller such as the
  sustain pedal, pitch bend, or pressure. Values keep MIDI 1.0's ranges, so any
  event can go to a plugin as MIDI without losing anything. `TimedEvent` puts
  one at a frame inside the block.
- `SustainPedal` plays the pedal for an instrument that only understands
  notes: it holds note offs back while the pedal is down and releases them
  when it comes up. Striking a held key again releases it first, and All Notes
  Off releases everything on the channel.
- `render_offline` drives a `Processor` without a device, in blocks of a chosen
  size, the way a device would, and hands each block the events that fall in
  it.

### `refrain-clap`

Hosts CLAP plugins, using [`clack-host`](https://github.com/prokopyl/clack).

- `ClapPlugin` lives on the main thread. It instantiates the plugin, activates
  it for a sample rate and block size, and runs the plugin's main-thread
  callback when the plugin asks for one.
- Activating gives a `ClapProcessor`, which is a `Processor` and moves to the
  audio thread. It starts processing on its first block, on the audio thread,
  as CLAP requires. Events go out in the dialects the plugin's note port
  understands. A plugin that takes MIDI gets notes as CLAP notes, when it
  takes those too, and everything else as MIDI, so it plays the pedal itself.
  A plugin that takes CLAP notes only gets notes, with `SustainPedal` playing
  the pedal for it. The processor then interleaves the plugin's main output:
  a mono plugin goes to every channel, a stereo plugin on a mono device is
  mixed down, and extra device channels stay silent.
- The audio stream has to stop before the `ClapPlugin` is dropped. If the
  processor is still alive at that point, clack leaks the instance instead of
  destroying it under the audio thread.

### `refrain-vst3`

Hosts VST3 plugins, using the [`vst3`](https://github.com/coupler-rs/vst3-rs)
bindings.

- `Vst3Module` is a loaded `.vst3` bundle (macOS) or file (Windows). On macOS
  it hands the module a real `CFBundleRef` in `bundleEntry`, as plugins expect.
- `Vst3Plugin` lives on the main thread. It creates the component and the
  controller, connects them through their connection points, passes the
  component's state to the controller, and activates the main buses.
- Activating gives a `Vst3Processor`, a `Processor` for the audio thread. It
  calls `setProcessing` on its first block, on the audio thread.
- VST3 has no MIDI controller events. At activation the host reads the
  plugin's `IMidiMapping` for every channel and controller, pitch bend and
  channel pressure included. Mapped controllers are sent as parameter changes
  at their sample offsets; everything else goes through `SustainPedal`, as for
  CLAP plugins that take notes only.
- The event list and parameter changes handed to `process` are COM objects
  that refrain implements. They are allocated at activation and reused.
- As with CLAP, the stream has to stop before the `Vst3Plugin` is dropped;
  otherwise the plugin is leaked rather than torn down under the audio thread.

### Plugin editors

`refrain play` and `refrain editor` open a plugin's editor in a window made
with [`winit`](https://github.com/rust-windowing/winit), on the main thread.
The plugin embeds its editor in the window's `NSView` (macOS) or `HWND`
(Windows): through the CLAP `gui` extension, or a VST3 `IPlugView`. Editor
sizes are in points on macOS and pixels on Windows, in both formats.

- When the plugin asks for a new size (CLAP `request_resize`, VST3
  `IPlugFrame::resizeView`), the window is resized on the next turn of the
  main loop and the plugin is told the size it got.
- A VST3 plugin reports edits made in its editor through the component
  handler. refrain passes them to the processor through a lock-free queue,
  and they take effect at the start of the next block. CLAP plugins apply
  their own edits.
- After opening, refrain checks that the window gained a child view or
  window, and says so: a plugin that accepts the window but puts nothing in
  it would otherwise show an empty window without explanation.
- The editor is closed before the window it lives in, and before the plugin.

### Test plugins

`refrain-test-tone` is a small sine synth. `refrain-test-synth` (CLAP) and
`refrain-test-synth-vst3` (VST3) both play it, built in this repository so
plugin hosting can be tested without third-party binaries. Each note is a sine wave at its pitch
with a 5 ms fade in and out, and the right channel is the left one at half
level. Being deterministic, it lets tests check exact answers. Being lopsided,
it shows swapped or duplicated channels.

Each format has two plugins that sound the same. One takes controllers and
plays the sustain pedal, pitch bend and All Notes Off itself: the CLAP one as
MIDI, the VST3 one as parameters it maps through `IMidiMapping`. The other
takes notes only, so refrain plays the pedal for it. Tests send all four the
same events and require the same samples. That checks refrain's pedal against
the plugins' own, and the CLAP host and the VST3 host against each other.

They have an editor too, from `refrain-test-editor`: a fixed 400 × 300 panel
with the plugin's name, added as a child of the host's window. It checks that
it really became a child, and fails otherwise, so a host that passes the wrong
window cannot pass the editor check in CI.

`scripts/build-test-plugins.sh` packages them the way plugins are installed,
as bundles on macOS and single files on Windows, for CI and for trying them.

Tests load them two ways: in-process, and in CI from their packaged files, the
way a real plugin is loaded.

### `refrain`

The `refrain` command. It owns everything that touches the platform:

- audio devices, through `cpal` (CoreAudio on macOS, WASAPI on Windows),
- MIDI input, through `midir`, feeding the audio thread through an `rtrb`
  queue,
- WAV files, through `hound`.

The device callback splits the device's buffer into blocks of at most
`MAX_BLOCK_FRAMES`, runs the engine into an `f32` buffer and converts it to
the device's sample type.

## Known limits

- **Note timing.** A note from the MIDI thread starts at the beginning of the
  next block, not at the moment it was played. At 48 kHz with 512-frame
  buffers that is up to about 11 ms of jitter.
- **Notes-only plugins** get no pitch bend, pressure or controllers other than
  the pedal. CLAP's note expressions could carry some of these; refrain does
  not send them yet.
- **Plugin logs** are printed from whichever thread logs, which can be the
  audio thread.
- **Editors are embedded only.** CLAP plugins whose editor can only float in a
  window of its own are not supported yet, and neither is the CLAP timer
  extension, which some plugins use to redraw on Linux.
- **Moving an editor's window between screens of different scale** does not
  tell the plugin about the new scale yet.
- **VST3 messages.** refrain does not create `IMessage` objects for plugins.
  The component and controller talk through their connection points directly,
  which is enough for the plugins tried so far.
- **Scanning loads every plugin in this process.** A plugin that crashes while
  being listed takes refrain down with it.

## Why the offline path matters

CI machines have no sound card. The engine and plugin hosting are tested
through `render_offline` instead, and that path is held to the same rules as a
device:

- **The output does not depend on the block size.** Rendering in blocks of 1,
  64, 100, 128, 1000 or 1024 frames gives the same samples bit for bit, with
  or without a plugin, with notes starting and stopping inside blocks. A
  device picks the block size, so anything that changes with it is a bug.
- **Tests catch the bugs they are meant to catch.** The hosting tests were
  checked by planting bugs in the host: dropping event offsets, interleaving
  the wrong channel, skipping the mono mixdown, dropping controllers on the
  way to a MIDI plugin, skipping the host's pedal, not releasing a held key
  when it is struck again, ignoring All Notes Off, and treating a pedal value
  of 64 as up. For VST3: not sending mapped controllers, dropping the sample
  offsets of notes or of parameter changes, normalizing pitch bend over 16384,
  and never reading the MIDI mapping, and dropping edits made in the editor.
  Each one fails a test. For editors, the host was made to skip handing the
  plugin its window; the check that the window gained a child caught it. One of them first passed, because the
  test synth hid the host's mistake; the synth was changed so it no longer
  does.

## Platform code stays thin

Everything that differs between macOS and Windows sits at the edge:
audio devices, MIDI ports, window handles for plugin editors (`NSView` and
`HWND`) and plugin search paths. The engine, the graph, the transport, project
files and most of plugin hosting are shared.
