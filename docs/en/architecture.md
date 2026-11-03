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
  `process(out, channels, events)` again and again. On entry `out` holds the
  processor's input, what came before it in a chain or else silence: an
  instrument replaces it, an effect changes it.
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
- `render_until_quiet` does the same and then goes on until the sound dies
  away: until a `Tail`'s number of frames in a row are no louder than its
  threshold, or its longest tail is reached. The quiet frames at the end are
  cut off, but never the frames asked for. Where it ends does not depend on
  the block size either.

### Tracks and the mix

A `Track` is a chain of processors, an instrument followed by its effects,
and a `Mix`: a volume in decibels, a pan, mute and solo. A `Mixer` is a
`Processor` that plays every track into a buffer of its own, adds them up,
and runs the sum through the master's chain of effects and the master volume.

- Each processor in a chain gets the buffer the one before it filled. The
  order is the order in the project, and the test effect makes it audible.
- Live events go to one track, the live track, and only to its instrument.
  Effects get no events.
- Pan turns one side down and leaves the other alone: at the centre a track
  is exactly its volume, so a track with the default mix passes through bit
  for bit. Only stereo has a pan; other channel counts take the volume only.
- When any track is soloed, only soloed tracks are heard, and mute wins over
  solo. A muted track is still played, so its instrument and clips stay in
  time and it comes back in the right place.
- Track buffers are allocated in `prepare`, for up to `MAX_CHANNELS` (32)
  channels; with more, the mixer gives silence rather than allocate on the
  audio thread.

`deinterleave` is the other way round from `interleave`: it turns the
interleaved signal into a plugin's channels for its input. A mono signal goes
to every input channel, a mono plugin gets a mixdown, and input channels the
signal lacks get silence.

### The timeline

Clips keep their events in ticks, 960 to a beat, so a change of tempo keeps
them in time with the music. The engine's timeline module holds:

- `Tempo`, which turns ticks into seconds and frames and back.
- `Sequencer`, a `Processor` that wraps another: it plays a sequence of
  events at exact frames from the start, merged with the events it is given
  live, and can add a metronome. Like the rest of the engine it gives the same
  samples for any block size.
- `Recording`, which collects events with the seconds they arrived at. When
  it finishes, notes still held are released and pedals still down come up,
  so a clip never leaves a note hanging when played back.

`refrain record` plays the project's clips through the instrument and records
what comes in from MIDI as a new clip. The MIDI thread stamps each event with
the moment it arrived, before it reaches the audio thread, so the recording is
as exact as the MIDI timestamps, not the block size. Recorded seconds become
ticks at the project's tempo. The metronome is mixed into the output and is
never recorded.

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
  the pedal for it. An effect gets what comes before it in the chain on its
  main audio input; other inputs, such as side chains, get silence. The
  processor then interleaves the plugin's main output:
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
  calls `setProcessing` on its first block, on the audio thread. An effect
  gets what comes before it in the chain on its first audio input; other
  inputs get silence.
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

### Projects and plugin state

A plugin's state is bytes only the plugin understands. refrain saves and
restores them, on the main thread:

- CLAP: the `state` extension's `save` and `load`.
- VST3: the component's `getState` and `setState`, and the controller's.
  When restoring, the controller is also given the component's state through
  `setComponentState`, so its parameters match.

`refrain-project` writes a project as a small JSON file. Version 3 holds
tracks, each with an instrument, its effects, its mix and its clips, and a
master with its effects and volume. Every plugin is named by its format, path
and ID, with its state in base64; an empty state means the plugin starts as it
would on its own. Versions 1 and 2 held one instrument and its clips, and read
as a project with that one track.

A relative plugin path is relative to the project's folder. When a project is
saved somewhere else, or tracks are added from another project, every
relative path is rewritten to lead to the same file from the new place, so
projects can move with the plugins next to them. A project is checked before
it is written, so refrain never writes one it would refuse to read (a volume
of infinity, for one, has no JSON). It is written to a temporary file first
and renamed over the old one, so a failed save leaves the old project whole.

VST3 class IDs are stored the way the VST3 SDK prints them. Windows lays class
IDs out in memory as COM GUIDs, with the first eight bytes in another order;
printing the raw bytes would give a different ID there, and a project saved on
a Mac would not find its plugin on Windows.

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

When the host tells them they render offline, the CLAP plugins through the
`render` extension and the VST3 ones through their process mode, they note
it, so tests can see it arrived. The VST3 plugins also fail any block whose
process mode differs from the one they were set up with.

They save their volume, set from MIDI controller 7, as their state. The state
format is the same in both formats, so a state saved through one host can be
restored through the other: a test that both hosts pass the bytes intact.

Each format also has an effect that multiplies by a gain, 2 to begin with,
and clips at ±0.25, saving its gain as its state. Because it clips, two of
them in a row give a different result in the other order, so a chain run in
the wrong order fails a test; and so does an effect that is fed silence, or
skipped. Tests run chains of effects in either format and in a mix of both,
and require the same samples.

The instruments have an editor too, from `refrain-test-editor`: a fixed 400 × 300 panel
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

`refrain render` tells every plugin it is rendering offline (CLAP's `render`
extension, VST3's `kOffline` process mode), renders the project's tracks to
the end of the last event, and then until the sound has been 100 dB down for
2 seconds, for at most 60. The file is 32-bit float, or 16- or 24-bit
integers, rounded to the nearest step and clipped at full scale. Dither, when
asked for, is triangular noise of one step from a fixed seed, so the same
project always gives the same file.

A `Session` is a project with all its plugins loaded and their states given
back. It builds the `Mixer` for playing or rendering, with each track's
instrument wrapped in a `Sequencer` that plays the track's clips, and saves
every plugin's state back into the project. A single plugin file opens as a
project of one track. `new`, `show`, `track`, `fx` and `mix` change a project
file without playing it; they load a plugin only to learn its format, ID and
name.

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
- **Recording is not lined up with what was heard.** Times are measured from
  when the audio stream started, without the output latency, so a recording
  lands a few milliseconds late. There is no count-in yet, every clip starts at
  the start of the timeline, and the tempo is one tempo for the whole project.
- **Effects add latency that is not made up for.** A plugin that reports
  latency is not delayed against the others yet.
- **Rendering** writes WAV only, at the sample rate asked for: there is no
  sample rate conversion, no normalizing, and no rendering of single tracks
  (stems). A plugin with a hard real-time requirement is still rendered
  offline.
- **The mix is fixed while playing.** Tracks, effects and the mix are set
  before playing starts; changing them means starting again. There are no
  sends, buses or groups, no automation, and effects get no MIDI and no side
  chain.
- **Projects are saved when the editor closes or `play` ends**, with `--save`.
  There is no saving while playing, and no autosave.
- **A project whose plugin has moved** is not found by ID in the installed
  plugins; refrain stops and names the path it looked at.
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
  Each one fails a test. For effects and the mix: feeding an effect silence
  instead of its input (in both hosts), running a chain backwards, swapping
  the sides of the pan, and not passing a project's mix to the mixer. For
  rendering: setting a VST3 plugin up offline but running its blocks in real
  time, and telling a CLAP plugin real time when offline was asked for. The pan
  swap first passed the engine's tests, whose levels added up the same with
  the sides swapped; the levels were changed so they no longer do. CI's mix
  check caught it, and the mix not being passed on, from the peaks of each
  channel. For state: handing a CLAP plugin an empty state,
  skipping a VST3 component's `setState`, and saving an empty VST3 state. For
  the timeline: playing sequenced events a frame late, dropping live events
  while a sequence plays, and leaving held notes in a finished recording. For
  editors, the host was made to skip handing the
  plugin its window; the check that the window gained a child caught it. One of them first passed, because the
  test synth hid the host's mistake; the synth was changed so it no longer
  does.

## Platform code stays thin

Everything that differs between macOS and Windows sits at the edge:
audio devices, MIDI ports, window handles for plugin editors (`NSView` and
`HWND`) and plugin search paths. The engine, the graph, the transport, project
files and most of plugin hosting are shared.
