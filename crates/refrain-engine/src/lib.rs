//! The real-time audio engine of refrain.
//!
//! Nothing in this crate talks to an OS or an audio device. The same code runs
//! inside a device callback and inside [`render_offline`], which is what lets
//! CI check the engine on machines that have no sound card.

/// The largest block, in frames, that a [`Processor`] is ever given.
///
/// Devices may ask for more at once. Drivers split their buffers into blocks
/// of at most this size, so a processor can size its buffers in `prepare`.
pub const MAX_BLOCK_FRAMES: usize = 1024;

/// The most events a single block carries. Further events wait for the next
/// block, so buffers for them can be allocated up front.
pub const MAX_BLOCK_EVENTS: usize = 256;

mod event;
mod mixer;
mod timeline;

pub use event::{
    ALL_NOTES_OFF, Event, MAX_PEDAL_RELEASES, PITCH_BEND_CENTRE, SUSTAIN, SustainPedal, TimedEvent,
};
pub use mixer::{MAX_CHANNELS, Mix, Mixer, Track, gain_from_db};
pub use timeline::{Recording, Sequencer, TICKS_PER_BEAT, Tempo};

/// Something that fills audio buffers: an instrument, an effect, or a chain
/// or mix of them.
///
/// `process` is called on the audio thread. It must not allocate, lock, block
/// or do I/O: a 128-frame block at 48 kHz has about 2.7 ms to finish.
pub trait Processor: Send {
    /// Called before the first `process`, and again whenever the sample rate
    /// changes. This is the place to allocate and to reset state. Blocks will
    /// hold at most `max_block_frames` frames.
    fn prepare(&mut self, sample_rate: f32, max_block_frames: usize);

    /// Fills `out`, which holds interleaved frames of `channels` samples each.
    /// `events` belong to this block and are in order of frame.
    ///
    /// On entry `out` holds the processor's input: what came before it in a
    /// chain, or silence. An instrument replaces it; an effect changes it.
    fn process(&mut self, out: &mut [f32], channels: usize, events: &[TimedEvent]);

    /// Whether the processor has failed and gives silence from now on, as a
    /// plugin does after an error.
    fn has_failed(&self) -> bool {
        false
    }
}

impl<P: Processor + ?Sized> Processor for Box<P> {
    fn prepare(&mut self, sample_rate: f32, max_block_frames: usize) {
        (**self).prepare(sample_rate, max_block_frames);
    }

    fn process(&mut self, out: &mut [f32], channels: usize, events: &[TimedEvent]) {
        (**self).process(out, channels, events);
    }

    fn has_failed(&self) -> bool {
        (**self).has_failed()
    }
}

/// A sine oscillator, written to every channel. It ignores events.
pub struct Sine {
    frequency: f64,
    amplitude: f32,
    sample_rate: f64,
    // Kept in [0, 1) and in f64, so the pitch does not drift on long runs.
    phase: f64,
}

impl Sine {
    pub fn new(frequency: f64, amplitude: f32) -> Self {
        Self {
            frequency,
            amplitude,
            sample_rate: 48_000.0,
            phase: 0.0,
        }
    }
}

impl Processor for Sine {
    fn prepare(&mut self, sample_rate: f32, _max_block_frames: usize) {
        self.sample_rate = f64::from(sample_rate);
        self.phase = 0.0;
    }

    fn process(&mut self, out: &mut [f32], channels: usize, _events: &[TimedEvent]) {
        let step = self.frequency / self.sample_rate;
        for frame in out.chunks_exact_mut(channels) {
            let value = self.amplitude * (std::f64::consts::TAU * self.phase).sin() as f32;
            frame.fill(value);
            self.phase += step;
            self.phase -= self.phase.floor();
        }
    }
}

/// Writes a plugin's output into an interleaved device buffer.
///
/// `planar` holds `plugin_channels` channels one after another, each
/// `stride` samples long; the first `out.len() / channels` samples of each are
/// used. A mono plugin goes to every channel, a plugin with more channels is
/// mixed down for a mono device, and device channels the plugin does not
/// have stay silent.
pub fn interleave(
    planar: &[f32],
    plugin_channels: usize,
    stride: usize,
    out: &mut [f32],
    channels: usize,
) {
    for (i, frame) in out.chunks_exact_mut(channels).enumerate() {
        if channels == 1 && plugin_channels > 1 {
            let sum: f32 = (0..plugin_channels).map(|c| planar[c * stride + i]).sum();
            frame[0] = sum / plugin_channels as f32;
            continue;
        }
        for (c, sample) in frame.iter_mut().enumerate() {
            *sample = match plugin_channels {
                1 => planar[i],
                n if c < n => planar[c * stride + i],
                _ => 0.0,
            };
        }
    }
}

/// Reads an interleaved device buffer into a plugin's input, the other way
/// round from [`interleave`].
///
/// `planar` gets `plugin_channels` channels one after another, each `stride`
/// samples long; the first `input.len() / channels` samples of each are
/// written. A mono input goes to every plugin channel, a mono plugin gets the
/// input mixed down, and plugin channels the input does not have get silence.
pub fn deinterleave(
    input: &[f32],
    channels: usize,
    planar: &mut [f32],
    plugin_channels: usize,
    stride: usize,
) {
    for (i, frame) in input.chunks_exact(channels).enumerate() {
        if plugin_channels == 1 && channels > 1 {
            planar[i] = frame.iter().sum::<f32>() / channels as f32;
            continue;
        }
        for c in 0..plugin_channels {
            planar[c * stride + i] = match channels {
                1 => frame[0],
                n if c < n => frame[c],
                _ => 0.0,
            };
        }
    }
}

/// Runs `processor` without a device and returns `frames` interleaved frames.
///
/// The work is split into blocks of `block_frames`, the way a device would
/// call it. `events` carry frames counted from the start of the render and
/// must be in order. A correct processor gives the same samples for any
/// block size.
pub fn render_offline(
    processor: &mut dyn Processor,
    sample_rate: f32,
    channels: usize,
    frames: usize,
    block_frames: usize,
    events: &[TimedEvent],
) -> Vec<f32> {
    assert!(channels > 0 && (1..=MAX_BLOCK_FRAMES).contains(&block_frames));
    assert!(events.is_sorted_by_key(|e| e.frame), "events out of order");
    let tail = Tail {
        threshold: 0.0,
        quiet_frames: 0,
        max_frames: 0,
    };
    render_until_quiet(
        processor,
        sample_rate,
        channels,
        frames,
        block_frames,
        events,
        tail,
    )
    .samples
}

/// How long to go on rendering after the end, for the sound to die away.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Tail {
    /// A sample whose size is at most this is quiet.
    pub threshold: f32,
    /// The sound has died away once this many frames in a row are quiet.
    pub quiet_frames: usize,
    /// The most frames to render after the end.
    pub max_frames: usize,
}

/// What [`render_until_quiet`] rendered.
pub struct Render {
    /// Interleaved frames.
    pub samples: Vec<f32>,
    /// False when the sound was still going after the longest tail.
    pub died_away: bool,
}

/// Like [`render_offline`], and then goes on past `frames` until the sound
/// dies away, or for at most `tail.max_frames`. The quiet frames at the end
/// are cut off, though never the first `frames`.
///
/// What is rendered does not depend on the block size, and neither does
/// where it ends.
pub fn render_until_quiet(
    processor: &mut dyn Processor,
    sample_rate: f32,
    channels: usize,
    frames: usize,
    block_frames: usize,
    events: &[TimedEvent],
    tail: Tail,
) -> Render {
    assert!(channels > 0 && (1..=MAX_BLOCK_FRAMES).contains(&block_frames));
    assert!(events.is_sorted_by_key(|e| e.frame), "events out of order");
    processor.prepare(sample_rate, block_frames);
    let limit = frames + tail.max_frames;
    let mut out = Vec::with_capacity(frames * channels);
    let mut pending = events.iter().peekable();
    let mut block_events = Vec::with_capacity(MAX_BLOCK_EVENTS);
    let mut start = 0;
    // The frame after the last loud one, and how many quiet frames follow it.
    let mut loud_end = 0;
    let mut quiet = 0;
    loop {
        let past_the_end = start >= frames;
        if past_the_end && (quiet >= tail.quiet_frames || start >= limit) {
            break;
        }
        // Blocks line up with the block size from the start, so the first
        // `frames` are cut into blocks as `render_offline` cuts them.
        let until = if past_the_end { limit } else { frames };
        let n = block_frames.min(until - start);
        let end = start + n;
        block_events.clear();
        while let Some(e) = pending.next_if(|e| (e.frame as usize) < end) {
            block_events.push(TimedEvent {
                frame: (e.frame as usize).saturating_sub(start) as u32,
                event: e.event,
            });
        }
        out.resize(end * channels, 0.0);
        let block = &mut out[start * channels..];
        processor.process(block, channels, &block_events);
        for (i, frame) in block.chunks_exact(channels).enumerate() {
            if frame.iter().any(|s| s.abs() > tail.threshold) {
                loud_end = start + i + 1;
                quiet = 0;
            } else {
                quiet += 1;
            }
        }
        start = end;
    }
    let died_away = quiet >= tail.quiet_frames || frames >= limit;
    out.truncate(loud_end.max(frames) * channels);
    Render {
        samples: out,
        died_away,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const RATE: f32 = 48_000.0;

    fn render_sine(amplitude: f32, channels: usize, frames: usize, block: usize) -> Vec<f32> {
        render_offline(
            &mut Sine::new(440.0, amplitude),
            RATE,
            channels,
            frames,
            block,
            &[],
        )
    }

    #[test]
    fn output_does_not_depend_on_block_size() {
        let reference = render_sine(0.5, 2, 4_800, 1_000);
        for block in [1, 64, 128, 480, 1_024] {
            assert_eq!(
                render_sine(0.5, 2, 4_800, block),
                reference,
                "block size {block}"
            );
        }
    }

    #[test]
    fn one_second_of_440_hz_has_440_cycles() {
        let out = render_sine(0.5, 1, RATE as usize, 128);
        // A cycle starts where the wave crosses zero going up.
        let rising = out.windows(2).filter(|w| w[0] < 0.0 && w[1] >= 0.0).count();
        // The first cycle starts at sample 0 and has no crossing before it.
        assert_eq!(rising + 1, 440);
    }

    #[test]
    fn every_channel_gets_the_same_sample_and_peaks_at_the_amplitude() {
        let out = render_sine(0.25, 3, 4_800, 128);
        for frame in out.as_chunks::<3>().0 {
            assert!(frame.iter().all(|&s| s == frame[0]));
        }
        let peak = out.iter().fold(0.0_f32, |m, s| m.max(s.abs()));
        assert!((peak - 0.25).abs() < 1e-3, "peak {peak}");
    }

    #[test]
    fn prepare_starts_over() {
        let mut sine = Sine::new(440.0, 0.5);
        let first = render_offline(&mut sine, RATE, 1, 1_000, 128, &[]);
        let second = render_offline(&mut sine, RATE, 1, 1_000, 128, &[]);
        assert_eq!(first, second);
    }

    #[test]
    fn deinterleave_undoes_interleave() {
        // Two frames of a stereo device buffer.
        let device = [0.125, 0.25, 0.375, 0.5];
        let mut planar = [9.0; 8];
        deinterleave(&device, 2, &mut planar, 2, 4);
        assert_eq!(planar, [0.125, 0.375, 9.0, 9.0, 0.25, 0.5, 9.0, 9.0]);
        let mut back = [0.0; 4];
        interleave(&planar, 2, 4, &mut back, 2);
        assert_eq!(back, device);

        // A mono plugin hears both channels; a stereo plugin hears a mono
        // input on both; a plugin channel the input lacks is silent.
        let mut mono = [0.0; 2];
        deinterleave(&device, 2, &mut mono, 1, 2);
        assert_eq!(mono, [0.1875, 0.4375]);
        let mut stereo = [9.0; 4];
        deinterleave(&[0.5, 0.25], 1, &mut stereo, 2, 2);
        assert_eq!(stereo, [0.5, 0.25, 0.5, 0.25]);
        let mut three = [9.0; 3];
        deinterleave(&[0.1, 0.2], 2, &mut three, 3, 1);
        assert_eq!(three, [0.1, 0.2, 0.0]);
    }

    /// Starts at `level` and decays by `factor` every frame.
    struct Decay {
        level: f32,
        factor: f32,
        start: f32,
    }

    impl Processor for Decay {
        fn prepare(&mut self, _: f32, _: usize) {
            self.level = self.start;
        }
        fn process(&mut self, out: &mut [f32], channels: usize, _: &[TimedEvent]) {
            for frame in out.chunks_exact_mut(channels) {
                frame.fill(self.level);
                self.level *= self.factor;
            }
        }
    }

    fn decay() -> Decay {
        Decay {
            level: 1.0,
            factor: 0.5,
            start: 1.0,
        }
    }

    const TAIL: Tail = Tail {
        threshold: 0.001,
        quiet_frames: 100,
        max_frames: 10_000,
    };

    #[test]
    fn the_tail_ends_where_the_sound_dies_away() {
        // 0.5^9 is above 0.001 and 0.5^10 is not: ten loud frames.
        for block in [1, 3, 128, 1_024] {
            let r = render_until_quiet(&mut decay(), RATE, 2, 4, block, &[], TAIL);
            assert!(r.died_away);
            assert_eq!(r.samples.len(), 10 * 2, "block size {block}");
            assert_eq!(r.samples[18], 0.5f32.powi(9));
        }
        // The frames asked for are kept, quiet or not.
        let r = render_until_quiet(&mut decay(), RATE, 1, 50, 16, &[], TAIL);
        assert_eq!(r.samples.len(), 50);
    }

    #[test]
    fn a_sound_that_never_dies_away_stops_at_the_longest_tail() {
        let mut sine = Sine::new(440.0, 0.5);
        let r = render_until_quiet(&mut sine, RATE, 1, 1_000, 128, &[], TAIL);
        assert!(!r.died_away);
        assert_eq!(r.samples.len(), 1_000 + TAIL.max_frames);
        // The first frames are what render_offline gives.
        let plain = render_offline(&mut Sine::new(440.0, 0.5), RATE, 1, 1_000, 128, &[]);
        assert_eq!(r.samples[..1_000], plain[..]);
    }

    /// Loud for 10 frames, quiet for `gap`, then loud for 10 more.
    struct Gap(usize, usize);

    impl Processor for Gap {
        fn prepare(&mut self, _: f32, _: usize) {
            self.1 = 0;
        }
        fn process(&mut self, out: &mut [f32], _: usize, _: &[TimedEvent]) {
            for s in out {
                let t = self.1;
                *s = if t < 10 || (10 + self.0..20 + self.0).contains(&t) {
                    0.5
                } else {
                    0.0
                };
                self.1 += 1;
            }
        }
    }

    #[test]
    fn a_pause_shorter_than_the_quiet_window_is_not_the_end() {
        let r = render_until_quiet(&mut Gap(50, 0), RATE, 1, 1, 32, &[], TAIL);
        assert_eq!(r.samples.len(), 70);
        // A longer pause is.
        let r = render_until_quiet(&mut Gap(200, 0), RATE, 1, 1, 32, &[], TAIL);
        assert_eq!(r.samples.len(), 10);
    }

    /// Records the events it is given, with frames counted from the start.
    struct Recorder {
        seen: Vec<TimedEvent>,
        elapsed: u32,
    }

    impl Processor for Recorder {
        fn prepare(&mut self, _: f32, _: usize) {}
        fn process(&mut self, out: &mut [f32], channels: usize, events: &[TimedEvent]) {
            for e in events {
                assert!(
                    (e.frame as usize) < out.len() / channels,
                    "event past the block"
                );
                self.seen.push(TimedEvent {
                    frame: e.frame + self.elapsed,
                    ..*e
                });
            }
            self.elapsed += (out.len() / channels) as u32;
        }
    }

    #[test]
    fn events_arrive_in_their_block_at_their_frame() {
        let on = Event::NoteOn {
            channel: 0,
            key: 60,
            velocity: 1.0,
        };
        let off = Event::NoteOff {
            channel: 0,
            key: 60,
            velocity: 1.0,
        };
        let events = [
            TimedEvent {
                frame: 0,
                event: on,
            },
            TimedEvent {
                frame: 127,
                event: off,
            },
            TimedEvent {
                frame: 128,
                event: on,
            },
            TimedEvent {
                frame: 999,
                event: off,
            },
        ];
        for block in [1, 64, 128, 1_000] {
            let mut recorder = Recorder {
                seen: Vec::new(),
                elapsed: 0,
            };
            render_offline(&mut recorder, RATE, 2, 1_000, block, &events);
            assert_eq!(recorder.seen, events, "block size {block}");
        }
    }
}
