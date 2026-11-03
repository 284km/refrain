//! Tracks, each an instrument and a chain of effects, mixed into one output
//! and played through the master chain, and the transport that plays their
//! sequences.

use crate::timeline::{Click, merge};
use crate::{ALL_NOTES_OFF, Event, MAX_BLOCK_EVENTS, Processor, SUSTAIN, Tempo, TimedEvent};

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

/// An instrument followed by its effects, how it goes into the mix, and the
/// sequence played on the instrument.
pub struct Track {
    // The instrument first. Each processor gets what the one before gave.
    chain: Vec<Box<dyn Processor>>,
    mix: Mix,
    buffer: Vec<f32>,
    // Frames from the start of the timeline, in order.
    sequence: Vec<(u64, Event)>,
    next: usize,
    block: Vec<TimedEvent>,
}

impl Track {
    /// `chain` is the instrument followed by its effects, in order.
    pub fn new(chain: Vec<Box<dyn Processor>>, mix: Mix) -> Self {
        Self {
            chain,
            mix,
            buffer: Vec::new(),
            sequence: Vec::new(),
            next: 0,
            block: Vec::with_capacity(MAX_BLOCK_EVENTS),
        }
    }

    /// Plays `sequence`, events at frames from the start of the timeline, on
    /// the instrument while the transport plays. It is sorted here.
    pub fn with_sequence(mut self, mut sequence: Vec<(u64, Event)>) -> Self {
        sequence.sort_by_key(|(frame, _)| *frame);
        self.sequence = sequence;
        self
    }
}

/// What a stop or a jump sends every instrument, so that no note goes on
/// sounding: the pedal up and All Notes Off, on every channel.
const RELEASE_EVENTS: usize = 32;
const _: () = assert!(RELEASE_EVENTS <= MAX_BLOCK_EVENTS);

fn release(block: &mut Vec<TimedEvent>) {
    for channel in 0..16 {
        for number in [SUSTAIN, ALL_NOTES_OFF] {
            block.push(TimedEvent {
                frame: 0,
                event: Event::Controller {
                    channel,
                    number,
                    value: 0,
                },
            });
        }
    }
}

/// Plays tracks, adds them up, and runs the sum through the master chain.
///
/// Live events go to the instrument of one track, the live track. Effects get
/// no events. Muted tracks are still played, so their instruments and
/// sequences stay in time; they are just not heard.
///
/// The mixer is also the transport. While it plays, the playhead moves and
/// the tracks' sequences play; while it is stopped, the playhead stands
/// still, and live events and the sound of the plugins go on. Stopping, or
/// moving the playhead, releases every instrument's notes. All of this, and
/// changes to the mix, may happen between any two blocks: none of it
/// allocates.
pub struct Mixer {
    tracks: Vec<Track>,
    master: Vec<Box<dyn Processor>>,
    master_gain: f32,
    live: Option<usize>,
    gains: Vec<(f32, f32)>,
    playing: bool,
    playhead: u64,
    click: Option<Click>,
    click_on: bool,
    // Send every instrument the release events in the next block.
    releasing: bool,
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
            playing: true,
            playhead: 0,
            click: None,
            click_on: false,
            releasing: false,
        }
    }

    /// Sends live events to the instrument of track `index`, counted from 0.
    pub fn with_live_track(mut self, index: usize) -> Self {
        self.live = Some(index);
        self
    }

    /// Adds a metronome at `tempo`, accenting the first of every
    /// `beats_per_bar` beats. It sounds while playing, on top of the mix.
    pub fn with_click(mut self, tempo: Tempo, beats_per_bar: u32) -> Self {
        self.click = Some(Click::new(tempo, beats_per_bar));
        self.click_on = true;
        self
    }

    /// Starts stopped, rather than playing.
    pub fn stopped(mut self) -> Self {
        self.playing = false;
        self
    }

    /// Changes which track live events go to, releasing the old one's notes
    /// at the next block.
    pub fn set_live_track(&mut self, index: usize) {
        if self.live != Some(index) {
            self.live = Some(index);
            self.releasing = true;
        }
    }

    pub fn play(&mut self) {
        self.playing = true;
    }

    /// Stops the playhead where it is, and releases every instrument's notes.
    pub fn stop(&mut self) {
        if self.playing {
            self.playing = false;
            self.releasing = true;
        }
    }

    pub fn is_playing(&self) -> bool {
        self.playing
    }

    /// Moves the playhead to `frame` from the start of the timeline, and
    /// releases every instrument's notes. Sequences go on from there.
    pub fn locate(&mut self, frame: u64) {
        self.playhead = frame;
        for track in &mut self.tracks {
            track.next = track.sequence.partition_point(|(f, _)| *f < frame);
        }
        self.releasing = true;
    }

    /// Frames from the start of the timeline to the playhead.
    pub fn playhead(&self) -> u64 {
        self.playhead
    }

    /// Turns the metronome on or off, if there is one.
    pub fn set_click(&mut self, on: bool) {
        self.click_on = on;
    }

    /// Changes how track `index` goes into the mix, from the next block.
    pub fn set_mix(&mut self, index: usize, mix: Mix) {
        if let Some(track) = self.tracks.get_mut(index) {
            track.mix = mix;
            self.gains[index] = mix.gains();
        }
    }

    pub fn set_master_volume(&mut self, db: f32) {
        self.master_gain = gain_from_db(db);
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
        if let Some(click) = &mut self.click {
            click.prepare(sample_rate);
        }
        self.playhead = 0;
        self.releasing = false;
        for track in &mut self.tracks {
            track.next = 0;
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
        let frames = (out.len() / channels) as u64;
        let start = self.playhead;
        let soloing = self.tracks.iter().any(|t| t.mix.solo);
        for (i, track) in self.tracks.iter_mut().enumerate() {
            let buffer = &mut track.buffer[..out.len()];
            buffer.fill(0.0);
            let live = if self.live == Some(i) { events } else { &[] };
            track.block.clear();
            if self.releasing {
                release(&mut track.block);
            }
            if self.playing {
                merge(
                    &track.sequence,
                    &mut track.next,
                    start..start + frames,
                    live,
                    &mut track.block,
                );
            } else {
                let room = MAX_BLOCK_EVENTS - track.block.len();
                track.block.extend(live.iter().take(room));
            }
            for (k, p) in track.chain.iter_mut().enumerate() {
                p.process(buffer, channels, if k == 0 { &track.block } else { &[] });
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
        if self.playing {
            if let (Some(click), true) = (&self.click, self.click_on) {
                click.add(out, channels, start);
            }
            self.playhead = start + frames;
        }
        self.releasing = false;
    }

    fn has_failed(&self) -> bool {
        self.processors().any(|p| p.has_failed())
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};

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

    /// Writes down the events it is given, at frames counted from its first
    /// block.
    struct Log(Arc<Mutex<Vec<(u64, Event)>>>, u64);

    impl Processor for Log {
        fn prepare(&mut self, _: f32, _: usize) {
            self.1 = 0;
        }
        fn process(&mut self, out: &mut [f32], channels: usize, events: &[TimedEvent]) {
            let mut log = self.0.lock().unwrap();
            log.extend(
                events
                    .iter()
                    .map(|e| (self.1 + u64::from(e.frame), e.event)),
            );
            self.1 += (out.len() / channels) as u64;
        }
    }

    type Shared = Arc<Mutex<Vec<(u64, Event)>>>;

    fn logged(sequence: Vec<(u64, Event)>) -> (Track, Shared) {
        let log = Shared::default();
        let track = track(vec![Box::new(Log(Arc::clone(&log), 0))]).with_sequence(sequence);
        (track, log)
    }

    fn note(key: u8) -> Event {
        Event::NoteOn {
            channel: 0,
            key,
            velocity: 1.0,
        }
    }

    fn is_release(e: &Event) -> bool {
        matches!(e, Event::Controller { number, .. } if *number == SUSTAIN || *number == ALL_NOTES_OFF)
    }

    /// Runs `blocks` blocks of `frames` frames.
    fn run(mixer: &mut Mixer, frames: usize, blocks: usize) {
        let mut out = vec![0.0; frames * 2];
        for _ in 0..blocks {
            mixer.process(&mut out, 2, &[]);
        }
    }

    #[test]
    fn sequences_play_on_their_own_tracks_at_their_frames() {
        for block in [1, 7, 128] {
            let (a, log_a) = logged(vec![(5, note(60)), (300, note(62))]);
            let (b, log_b) = logged(vec![(0, note(70)), (299, note(71))]);
            let mut mixer = Mixer::new(vec![a, b], Vec::new(), 0.0).with_live_track(0);
            let live = [TimedEvent {
                frame: 5,
                event: note(1),
            }];
            render_offline(&mut mixer, RATE, 2, 400, block, &live);
            // At a frame, live events go after sequenced ones.
            assert_eq!(
                *log_a.lock().unwrap(),
                [(5, note(60)), (5, note(1)), (300, note(62))],
                "block size {block}"
            );
            assert_eq!(*log_b.lock().unwrap(), [(0, note(70)), (299, note(71))]);
            assert_eq!(mixer.playhead(), 400);
        }
    }

    #[test]
    fn stopping_holds_the_playhead_and_releases_notes() {
        let (a, log) = logged(vec![(150, note(60))]);
        let mut mixer = Mixer::new(vec![a], Vec::new(), 0.0);
        mixer.prepare(RATE, 128);
        run(&mut mixer, 100, 1);
        mixer.stop();
        run(&mut mixer, 100, 3);
        assert_eq!(mixer.playhead(), 100);
        {
            let log = log.lock().unwrap();
            // Released once, at the first stopped block; the note waits.
            assert_eq!(log.len(), RELEASE_EVENTS);
            assert!(log.iter().all(|(frame, e)| *frame == 100 && is_release(e)));
        }
        mixer.play();
        run(&mut mixer, 100, 1);
        // The note at 150 on the timeline comes 50 frames into this block,
        // which the instrument counts as 450.
        assert_eq!(log.lock().unwrap().last(), Some(&(450, note(60))));
    }

    #[test]
    fn locating_skips_ahead_and_releases_notes() {
        let (a, log) = logged(vec![(10, note(60)), (1_000, note(62)), (1_050, note(64))]);
        let mut mixer = Mixer::new(vec![a], Vec::new(), 0.0);
        mixer.prepare(RATE, 128);
        mixer.locate(1_000);
        run(&mut mixer, 100, 1);
        let log = log.lock().unwrap();
        let notes: Vec<_> = log.iter().filter(|(_, e)| !is_release(e)).collect();
        assert_eq!(notes, [&(0, note(62)), &(50, note(64))]);
        assert_eq!(
            log.iter().filter(|(_, e)| is_release(e)).count(),
            RELEASE_EVENTS
        );
        assert_eq!(mixer.playhead(), 1_100);
    }

    #[test]
    fn live_events_play_while_stopped() {
        let (a, log) = logged(vec![(0, note(60))]);
        let mut mixer = Mixer::new(vec![a], Vec::new(), 0.0)
            .with_live_track(0)
            .stopped();
        mixer.prepare(RATE, 128);
        let mut out = vec![0.0; 256];
        mixer.process(
            &mut out,
            2,
            &[TimedEvent {
                frame: 3,
                event: note(1),
            }],
        );
        assert_eq!(*log.lock().unwrap(), [(3, note(1))]);
        assert_eq!(mixer.playhead(), 0);
    }

    #[test]
    fn the_click_sounds_only_while_playing() {
        let click = || {
            Mixer::new(vec![track(vec![Box::new(Level(0.0))])], Vec::new(), 0.0)
                .with_click(Tempo::new(120.0), 4)
        };
        let heard = |mixer: &mut Mixer| {
            let out = render_offline(mixer, RATE, 2, 1_000, 128, &[]);
            out.iter().any(|&s| s != 0.0)
        };
        assert!(heard(&mut click()));
        assert!(!heard(&mut click().stopped()));
        let mut off = click();
        off.set_click(false);
        assert!(!heard(&mut off));
    }

    #[test]
    fn changing_the_live_track_releases_the_old_ones_notes() {
        let (a, log_a) = logged(Vec::new());
        let (b, log_b) = logged(Vec::new());
        let mut mixer = Mixer::new(vec![a, b], Vec::new(), 0.0).with_live_track(0);
        mixer.prepare(RATE, 128);
        mixer.set_live_track(1);
        run(&mut mixer, 10, 1);
        assert_eq!(log_a.lock().unwrap().len(), RELEASE_EVENTS);
        assert_eq!(log_b.lock().unwrap().len(), RELEASE_EVENTS);
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
        a.set_mix(0, Mix::default());
        a.process(&mut out, 2, &[]);
        let reference = render_offline(&mut Sine::new(440.0, 0.5), RATE, 2, 256, 128, &[]);
        assert_eq!(out, reference[256..]);
    }
}
