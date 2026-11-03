# Architecture

This page describes how refrain is put together today and the rules it keeps.
It grows as features land.

## Two threads, two sets of rules

```
UI / control thread ──(lock-free queue)──▶ audio thread
  devices, files, plugin windows             transport, graph, plugin process()
  may allocate, lock, block                  must not allocate, lock, block or do I/O
```

The audio thread has a deadline. A 128-frame block at 48 kHz must be finished
in about 2.7 ms, every time. Missing it is an audible click. So code that runs
there:

- does not allocate or free memory,
- does not take locks that the other thread can hold,
- does not do I/O, print, or wait.

Memory is allocated in `Processor::prepare`, which runs off the audio thread,
before the first block and whenever the sample rate changes.

## Crates

### `refrain-engine`

The engine. It has no OS, device or file code, and no dependencies.

- `Processor` is anything that fills an interleaved `f32` buffer:
  `prepare(sample_rate)` then `process(out, channels)` again and again.
- `render_offline` drives a `Processor` without a device, in blocks of a chosen
  size, the way a device would.

### `refrain`

The `refrain` command. It owns everything that touches the platform:

- audio devices, through `cpal` (CoreAudio on macOS, WASAPI on Windows),
- WAV files, through `hound`.

When a device asks for a sample type other than `f32`, the callback runs the
engine into a fixed buffer that lives inside the callback and converts from
there, so the audio thread still does not allocate.

## Why the offline path matters

CI machines have no sound card. The engine is tested through `render_offline`
instead, and that path is held to the same rule as a device:

- **The output does not depend on the block size.** Rendering in blocks of 1,
  64, 128, 480 or 1000 frames gives the same samples bit for bit. A device picks
  the block size, so anything that changes with it is a bug.

## Platform code stays thin

Everything that differs between macOS and Windows sits at the edge:
audio devices, MIDI ports, window handles for plugin editors (`NSView` and
`HWND`) and plugin search paths. The engine, the graph, the transport, project
files and most of plugin hosting are shared.
