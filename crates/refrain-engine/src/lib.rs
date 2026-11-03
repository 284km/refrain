//! The real-time audio engine of refrain.
//!
//! Nothing in this crate talks to an OS or an audio device. The same code runs
//! inside a device callback and inside [`render_offline`], which is what lets
//! CI check the engine on machines that have no sound card.

/// Something that fills audio buffers.
///
/// `process` is called on the audio thread. It must not allocate, lock, block
/// or do I/O: a 128-frame block at 48 kHz has about 2.7 ms to finish.
pub trait Processor: Send {
    /// Called before the first `process`, and again whenever the sample rate
    /// changes. This is the place to allocate and to reset state.
    fn prepare(&mut self, sample_rate: f32);

    /// Fills `out`, which holds interleaved frames of `channels` samples each.
    fn process(&mut self, out: &mut [f32], channels: usize);
}

/// A sine oscillator, written to every channel.
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
    fn prepare(&mut self, sample_rate: f32) {
        self.sample_rate = f64::from(sample_rate);
        self.phase = 0.0;
    }

    fn process(&mut self, out: &mut [f32], channels: usize) {
        let step = self.frequency / self.sample_rate;
        for frame in out.chunks_exact_mut(channels) {
            let value = self.amplitude * (std::f64::consts::TAU * self.phase).sin() as f32;
            frame.fill(value);
            self.phase += step;
            self.phase -= self.phase.floor();
        }
    }
}

/// Runs `processor` without a device and returns `frames` interleaved frames.
///
/// The work is split into blocks of `block_frames`, the way a device would
/// call it. A correct processor gives the same samples for any block size.
pub fn render_offline(
    processor: &mut dyn Processor,
    sample_rate: f32,
    channels: usize,
    frames: usize,
    block_frames: usize,
) -> Vec<f32> {
    assert!(channels > 0 && block_frames > 0);
    processor.prepare(sample_rate);
    let mut out = vec![0.0; frames * channels];
    for block in out.chunks_mut(block_frames * channels) {
        processor.process(block, channels);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const RATE: f32 = 48_000.0;

    #[test]
    fn output_does_not_depend_on_block_size() {
        let reference = render_offline(&mut Sine::new(440.0, 0.5), RATE, 2, 4_800, 4_800);
        for block in [1, 64, 128, 480, 1_000] {
            let split = render_offline(&mut Sine::new(440.0, 0.5), RATE, 2, 4_800, block);
            assert_eq!(split, reference, "block size {block}");
        }
    }

    #[test]
    fn one_second_of_440_hz_has_440_cycles() {
        let out = render_offline(&mut Sine::new(440.0, 0.5), RATE, 1, RATE as usize, 128);
        // A cycle starts where the wave crosses zero going up.
        let rising = out.windows(2).filter(|w| w[0] < 0.0 && w[1] >= 0.0).count();
        // The first cycle starts at sample 0 and has no crossing before it.
        assert_eq!(rising + 1, 440);
    }

    #[test]
    fn every_channel_gets_the_same_sample_and_peaks_at_the_amplitude() {
        let out = render_offline(&mut Sine::new(440.0, 0.25), RATE, 3, 4_800, 128);
        for frame in out.as_chunks::<3>().0 {
            assert!(frame.iter().all(|&s| s == frame[0]));
        }
        let peak = out.iter().fold(0.0_f32, |m, s| m.max(s.abs()));
        assert!((peak - 0.25).abs() < 1e-3, "peak {peak}");
    }

    #[test]
    fn prepare_starts_over() {
        let mut sine = Sine::new(440.0, 0.5);
        let first = render_offline(&mut sine, RATE, 1, 1_000, 128);
        let second = render_offline(&mut sine, RATE, 1, 1_000, 128);
        assert_eq!(first, second);
    }
}
