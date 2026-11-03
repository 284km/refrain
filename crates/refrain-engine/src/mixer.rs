//! Tracks, each an instrument and a chain of effects, mixed into one output
//! and played through the master chain.

use crate::{Processor, TimedEvent};

/// The most channels a [`Mixer`] mixes. With more, it gives silence.
///
/// Tracks keep a buffer each, allocated in `prepare`, before the channel
/// count is known.
pub const MAX_CHANNELS: usize = 32;

/// How a track goes into the mix.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Mix {
    /// Volume in decibels; 0 leaves the track as it is.
    pub volume_db: f32,
    /// Balance between left (-1) and right (1); 0 is the centre. Only a
    /// stereo output has a balance; other channel counts ignore it.
    pub pan: f32,
    pub mute: bool,
    /// When any track is soloed, only soloed tracks are heard.
    pub solo: bool,
}

impl Default for Mix {
    fn default() -> Self {
        Self {
            volume_db: 0.0,
            pan: 0.0,
            mute: false,
            solo: false,
        }
    }
}

impl Mix {
    /// The gains of the left and right channels.
    ///
    /// Panning turns one side down and leaves the other as it is, so the
    /// centre is exactly the track's volume and a track with the default mix
    /// passes through unchanged.
    fn gains(&self) -> (f32, f32) {
        let gain = gain_from_db(self.volume_db);
        let pan = self.pan.clamp(-1.0, 1.0);
        (gain * (1.0 - pan).min(1.0), gain * (1.0 + pan).min(1.0))
    }
}

/// The factor a volume in decibels multiplies by. 0 dB is exactly 1.
pub fn gain_from_db(db: f32) -> f32 {
    10f32.powf(db / 20.0)
}

/// An instrument followed by its effects, and how it goes into the mix.
pub struct Track {
    // The instrument first. Each processor gets what the one before gave.
    chain: Vec<Box<dyn Processor>>,
    mix: Mix,
    buffer: Vec<f32>,
}

impl Track {
    /// `chain` is the instrument followed by its effects, in order.
    pub fn new(chain: Vec<Box<dyn Processor>>, mix: Mix) -> Self {
        Self {
            chain,
            mix,
            buffer: Vec::new(),
        }
    }
}

/// Plays tracks, adds them up, and runs the sum through the master chain.
///
/// Live events go to the instrument of one track, the live track. Effects get
/// no events. Muted tracks are still played, so their instruments and
/// sequences stay in time; they are just not heard.
pub struct Mixer {
    tracks: Vec<Track>,
    master: Vec<Box<dyn Processor>>,
    master_gain: f32,
    live: Option<usize>,
    gains: Vec<(f32, f32)>,
}

impl Mixer {
    /// `master` is the chain of effects after the sum, in order, and
    /// `master_volume_db` the volume after them.
    pub fn new(tracks: Vec<Track>, master: Vec<Box<dyn Processor>>, master_volume_db: f32) -> Self {
        let gains = tracks.iter().map(|t| t.mix.gains()).collect();
        Self {
            tracks,
            master,
            master_gain: gain_from_db(master_volume_db),
            live: None,
            gains,
        }
    }

    /// Sends live events to the instrument of track `index`, counted from 0.
    pub fn with_live_track(mut self, index: usize) -> Self {
        self.live = Some(index);
        self
    }

    fn processors(&self) -> impl Iterator<Item = &Box<dyn Processor>> {
        self.tracks
            .iter()
            .flat_map(|t| &t.chain)
            .chain(&self.master)
    }
}

impl Processor for Mixer {
    fn prepare(&mut self, sample_rate: f32, max_block_frames: usize) {
        for track in &mut self.tracks {
            track.buffer = vec![0.0; max_block_frames * MAX_CHANNELS];
            for p in &mut track.chain {
                p.prepare(sample_rate, max_block_frames);
            }
        }
        for p in &mut self.master {
            p.prepare(sample_rate, max_block_frames);
        }
    }

    fn process(&mut self, out: &mut [f32], channels: usize, events: &[TimedEvent]) {
        out.fill(0.0);
        if channels > MAX_CHANNELS {
            return;
        }
        let soloing = self.tracks.iter().any(|t| t.mix.solo);
        for (i, track) in self.tracks.iter_mut().enumerate() {
            let buffer = &mut track.buffer[..out.len()];
            buffer.fill(0.0);
            let live = if self.live == Some(i) { events } else { &[] };
            for (k, p) in track.chain.iter_mut().enumerate() {
                p.process(buffer, channels, if k == 0 { live } else { &[] });
            }
            if track.mix.mute || (soloing && !track.mix.solo) {
                continue;
            }
            let (left, right) = self.gains[i];
            if channels == 2 {
                for (o, b) in out
                    .as_chunks_mut::<2>()
                    .0
                    .iter_mut()
                    .zip(buffer.as_chunks::<2>().0)
                {
                    o[0] += b[0] * left;
                    o[1] += b[1] * right;
                }
            } else {
                for (o, b) in out.iter_mut().zip(buffer.iter()) {
                    *o += b * left.max(right);
                }
            }
        }
        for p in &mut self.master {
            p.process(out, channels, &[]);
        }
        if self.master_gain != 1.0 {
            for s in out.iter_mut() {
                *s *= self.master_gain;
            }
        }
    }

    fn has_failed(&self) -> bool {
        self.processors().any(|p| p.has_failed())
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;
    use crate::{Event, Sine, render_offline};

    const RATE: f32 = 48_000.0;

    /// An instrument that plays a constant.
    struct Level(f32);

    impl Processor for Level {
        fn prepare(&mut self, _: f32, _: usize) {}
        fn process(&mut self, out: &mut [f32], _: usize, _: &[TimedEvent]) {
            out.fill(self.0);
        }
    }

    /// Effects that add or multiply.
    struct Add(f32);
    struct Scale(f32);

    impl Processor for Add {
        fn prepare(&mut self, _: f32, _: usize) {}
        fn process(&mut self, out: &mut [f32], _: usize, _: &[TimedEvent]) {
            out.iter_mut().for_each(|s| *s += self.0);
        }
    }

    impl Processor for Scale {
        fn prepare(&mut self, _: f32, _: usize) {}
        fn process(&mut self, out: &mut [f32], _: usize, _: &[TimedEvent]) {
            out.iter_mut().for_each(|s| *s *= self.0);
        }
    }

    /// Counts the events it is given, and passes its input on.
    struct Heard(Arc<AtomicUsize>);

    impl Processor for Heard {
        fn prepare(&mut self, _: f32, _: usize) {}
        fn process(&mut self, _: &mut [f32], _: usize, events: &[TimedEvent]) {
            self.0.fetch_add(events.len(), Ordering::Relaxed);
        }
    }

    fn track(chain: Vec<Box<dyn Processor>>) -> Track {
        Track::new(chain, Mix::default())
    }

    fn render(mixer: &mut Mixer, channels: usize) -> Vec<f32> {
        render_offline(mixer, RATE, channels, 300, 128, &[])
    }

    #[test]
    fn a_track_with_the_default_mix_is_its_instrument() {
        let alone = render_offline(&mut Sine::new(440.0, 0.5), RATE, 2, 4_800, 128, &[]);
        let mut mixer = Mixer::new(
            vec![track(vec![Box::new(Sine::new(440.0, 0.5))])],
            Vec::new(),
            0.0,
        );
        for block in [1, 128, 1_000] {
            assert_eq!(
                render_offline(&mut mixer, RATE, 2, 4_800, block, &[]),
                alone,
                "block size {block}"
            );
        }
    }

    #[test]
    fn effects_take_what_comes_before_them_in_order() {
        let chain = |first: Box<dyn Processor>, second: Box<dyn Processor>| {
            let mut mixer = Mixer::new(
                vec![track(vec![Box::new(Level(0.25)), first, second])],
                Vec::new(),
                0.0,
            );
            render(&mut mixer, 2)[0]
        };
        assert_eq!(chain(Box::new(Add(0.5)), Box::new(Scale(2.0))), 1.5);
        assert_eq!(chain(Box::new(Scale(2.0)), Box::new(Add(0.5))), 1.0);
    }

    #[test]
    fn tracks_add_up_after_their_volume_and_pan() {
        let half = 20.0 * 0.5f32.log10();
        let mut mixer = Mixer::new(
            vec![
                Track::new(
                    vec![Box::new(Level(0.5))],
                    Mix {
                        volume_db: half,
                        ..Mix::default()
                    },
                ),
                // Right: the left side turned down by half.
                Track::new(
                    vec![Box::new(Level(0.25))],
                    Mix {
                        pan: 0.5,
                        ..Mix::default()
                    },
                ),
                Track::new(
                    // Hard left. Levels differ on purpose: with the sides
                    // swapped, the sums would differ too.
                    vec![Box::new(Level(0.0625))],
                    Mix {
                        pan: -1.0,
                        ..Mix::default()
                    },
                ),
            ],
            Vec::new(),
            0.0,
        );
        let out = render(&mut mixer, 2);
        let (left, right) = (out[0], out[1]);
        assert!((left - (0.25 + 0.125 + 0.0625)).abs() < 1e-6, "left {left}");
        assert!((right - (0.25 + 0.25)).abs() < 1e-6, "right {right}");
        // Every frame alike.
        assert!(out.as_chunks::<2>().0.iter().all(|f| *f == [left, right]));

        // A mono output has no balance, only volume.
        let out = render(&mut mixer, 1);
        assert!(
            (out[0] - (0.25 + 0.25 + 0.0625)).abs() < 1e-6,
            "mono {}",
            out[0]
        );
    }

    #[test]
    fn mute_silences_and_solo_picks() {
        let mix = |mixes: [Mix; 3]| {
            let tracks = [1.0, 2.0, 4.0]
                .into_iter()
                .zip(mixes)
                .map(|(level, mix)| Track::new(vec![Box::new(Level(level))], mix))
                .collect();
            render(&mut Mixer::new(tracks, Vec::new(), 0.0), 2)[0]
        };
        let plain = Mix::default();
        let mute = Mix {
            mute: true,
            ..plain
        };
        let solo = Mix {
            solo: true,
            ..plain
        };
        let both = Mix {
            mute: true,
            solo: true,
            ..plain
        };
        assert_eq!(mix([plain; 3]), 7.0);
        assert_eq!(mix([mute, plain, plain]), 6.0);
        assert_eq!(mix([plain, plain, solo]), 4.0);
        assert_eq!(mix([plain, solo, solo]), 6.0);
        // Muting a soloed track wins.
        assert_eq!(mix([plain, solo, both]), 2.0);
    }

    #[test]
    fn the_master_chain_takes_the_sum_then_the_master_volume() {
        let double = 20.0 * 2f32.log10();
        let mut mixer = Mixer::new(
            vec![
                track(vec![Box::new(Level(0.25))]),
                track(vec![Box::new(Level(0.25))]),
            ],
            vec![Box::new(Add(1.0))],
            double,
        );
        // (0.25 + 0.25 + 1) * 2; once per track it would be 5.
        let out = render(&mut mixer, 2)[0];
        assert!((out - 3.0).abs() < 1e-5, "{out}");
    }

    #[test]
    fn live_events_reach_only_the_live_tracks_instrument() {
        let counters: Vec<Arc<AtomicUsize>> = (0..4).map(|_| Arc::default()).collect();
        let heard = |i: usize| -> Box<dyn Processor> { Box::new(Heard(Arc::clone(&counters[i]))) };
        let mut mixer = Mixer::new(
            vec![track(vec![heard(0)]), track(vec![heard(1), heard(2)])],
            vec![heard(3)],
            0.0,
        )
        .with_live_track(1);
        let on = Event::NoteOn {
            channel: 0,
            key: 60,
            velocity: 1.0,
        };
        let events: Vec<TimedEvent> = (0..5)
            .map(|frame| TimedEvent { frame, event: on })
            .collect();
        render_offline(&mut mixer, RATE, 2, 300, 128, &events);
        let counts: Vec<usize> = counters.iter().map(|c| c.load(Ordering::Relaxed)).collect();
        assert_eq!(counts, [0, 5, 0, 0]);
    }

    #[test]
    fn a_muted_track_keeps_playing_unheard() {
        // A muted sine that is unmuted later picks up where it would be.
        let sine = || -> Box<dyn Processor> { Box::new(Sine::new(440.0, 0.5)) };
        let muted = Mix {
            mute: true,
            ..Mix::default()
        };
        let mut a = Mixer::new(vec![Track::new(vec![sine()], muted)], Vec::new(), 0.0);
        a.prepare(RATE, 128);
        let mut out = vec![1.0; 256];
        a.process(&mut out, 2, &[]);
        assert!(out.iter().all(|&s| s == 0.0));
        a.tracks[0].mix.mute = false;
        a.gains[0] = a.tracks[0].mix.gains();
        a.process(&mut out, 2, &[]);
        let reference = render_offline(&mut Sine::new(440.0, 0.5), RATE, 2, 256, 128, &[]);
        assert_eq!(out, reference[256..]);
    }
}
