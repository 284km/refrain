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
mod timeline;

pub use event::{
    ALL_NOTES_OFF, Event, MAX_PEDAL_RELEASES, PITCH_BEND_CENTRE, SUSTAIN, SustainPedal, TimedEvent,
};
pub use timeline::{Recording, Sequencer, TICKS_PER_BEAT, Tempo};

/// Something that fills audio buffers.
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
    processor.prepare(sample_rate, block_frames);
    let mut out = vec![0.0; frames * channels];
    let mut pending = events.iter().peekable();
    let mut block_events = Vec::with_capacity(MAX_BLOCK_EVENTS);
    for (index, block) in out.chunks_mut(block_frames * channels).enumerate() {
        let start = index * block_frames;
        let end = start + block.len() / channels;
        block_events.clear();
        while let Some(e) = pending.next_if(|e| (e.frame as usize) < end) {
            block_events.push(TimedEvent {
                frame: (e.frame as usize - start) as u32,
                event: e.event,
            });
        }
        processor.process(block, channels, &block_events);
    }
    out
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
