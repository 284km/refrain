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
- `NoteEvent` is a note on or off. `TimedEvent` puts one at a frame inside the
  block.
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
  as CLAP requires. It turns `NoteEvent`s into CLAP note events, or into MIDI
  for plugins that only take MIDI, and interleaves the plugin's main output:
  a mono plugin goes to every channel, a stereo plugin on a mono device is
  mixed down, and extra device channels stay silent.
- The audio stream has to stop before the `ClapPlugin` is dropped. If the
  processor is still alive at that point, clack leaks the instance instead of
  destroying it under the audio thread.

### `refrain-test-synth`

A small CLAP instrument, built in this repository so plugin hosting can be
tested without third-party binaries. Each note is a sine wave at its pitch
with a 5 ms fade in and out, and the right channel is the left one at half
level. Being deterministic, it lets tests check exact answers. Being lopsided,
it shows swapped or duplicated channels.

Tests load it two ways: in-process, through clack's support for Rust plugins,
and in CI from its built file, the way a real plugin is loaded.

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
- **Notes only.** The sustain pedal, pitch bend and other controllers do not
  reach plugins yet.
- **Plugin logs** are printed from whichever thread logs, which can be the
  audio thread.
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
  the wrong channel, and skipping the mono mixdown. Each one fails a test.

## Platform code stays thin

Everything that differs between macOS and Windows sits at the edge:
audio devices, MIDI ports, window handles for plugin editors (`NSView` and
`HWND`) and plugin search paths. The engine, the graph, the transport, project
files and most of plugin hosting are shared.
