//! Musical time, playing sequences, a metronome, and turning a recorded
//! performance into one that can be played back.

use crate::{Event, MAX_BLOCK_EVENTS, Processor, SUSTAIN, TimedEvent};

/// Ticks in a beat (a quarter note). Clips keep their events in ticks, so a
/// change of tempo keeps them in time with the music.
pub const TICKS_PER_BEAT: u64 = 960;

/// A constant tempo.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Tempo {
    pub beats_per_minute: f64,
}

impl Tempo {
    pub fn new(beats_per_minute: f64) -> Self {
        Self { beats_per_minute }
    }

    pub fn seconds_per_beat(&self) -> f64 {
        60.0 / self.beats_per_minute
    }

    /// The tick nearest to `seconds` from the start.
    pub fn ticks_at(&self, seconds: f64) -> u64 {
        (seconds.max(0.0) / self.seconds_per_beat() * TICKS_PER_BEAT as f64).round() as u64
    }

    pub fn seconds_at(&self, ticks: u64) -> f64 {
        ticks as f64 / TICKS_PER_BEAT as f64 * self.seconds_per_beat()
    }

    /// The frame nearest to `ticks` from the start, at `sample_rate`.
    pub fn frame_at(&self, ticks: u64, sample_rate: f32) -> u64 {
        (self.seconds_at(ticks) * f64::from(sample_rate)).round() as u64
    }
}

/// Plays a sequence of events into a processor, together with the events it
/// is given live, and adds a metronome if asked to.
///
/// The sequence starts at the first block. Its events land on their exact
/// frames, whatever the block size.
pub struct Sequencer<P> {
    inner: P,
    // Frames from the start, in order.
    sequence: Vec<(u64, Event)>,
    next: usize,
    playhead: u64,
    block: Vec<TimedEvent>,
    click: Option<Click>,
}

impl<P: Processor> Sequencer<P> {
    /// `sequence` holds events at frames from the start; it is sorted here.
    pub fn new(inner: P, mut sequence: Vec<(u64, Event)>) -> Self {
        sequence.sort_by_key(|(frame, _)| *frame);
        Self {
            inner,
            sequence,
            next: 0,
            playhead: 0,
            block: Vec::with_capacity(MAX_BLOCK_EVENTS),
            click: None,
        }
    }

    /// Adds a metronome at `tempo`, accenting the first of every
    /// `beats_per_bar` beats.
    pub fn with_click(mut self, tempo: Tempo, beats_per_bar: u32) -> Self {
        self.click = Some(Click::new(tempo, beats_per_bar));
        self
    }

    /// Frames played since the start.
    pub fn playhead(&self) -> u64 {
        self.playhead
    }
}

impl<P: Processor> Processor for Sequencer<P> {
    fn prepare(&mut self, sample_rate: f32, max_block_frames: usize) {
        self.inner.prepare(sample_rate, max_block_frames);
        if let Some(click) = &mut self.click {
            click.prepare(sample_rate);
        }
        self.next = 0;
        self.playhead = 0;
    }

    fn process(&mut self, out: &mut [f32], channels: usize, live: &[TimedEvent]) {
        let frames = (out.len() / channels) as u64;
        let end = self.playhead + frames;
        // Merge the sequence's events in this block with the live ones, in
        // order of frame. Live events go after sequenced ones at a frame.
        self.block.clear();
        let mut live = live.iter().peekable();
        while self.block.len() < MAX_BLOCK_EVENTS {
            let sequenced = self
                .sequence
                .get(self.next)
                .filter(|(frame, _)| *frame < end)
                .map(|&(frame, event)| TimedEvent {
                    frame: (frame - self.playhead) as u32,
                    event,
                });
            match (sequenced, live.peek()) {
                (Some(s), Some(l)) if l.frame < s.frame => self.block.push(*live.next().unwrap()),
                (Some(s), _) => {
                    self.block.push(s);
                    self.next += 1;
                }
                (None, Some(_)) => self.block.push(*live.next().unwrap()),
                (None, None) => break,
            }
        }
        self.inner.process(out, channels, &self.block);
        if let Some(click) = &mut self.click {
            click.add(out, channels, self.playhead);
        }
        self.playhead = end;
    }

    fn has_failed(&self) -> bool {
        self.inner.has_failed()
    }
}

/// A metronome: a short, decaying sine on every beat, higher on the first
/// beat of a bar. Its sound is added to the output, and never recorded.
struct Click {
    tempo: Tempo,
    beats_per_bar: u32,
    sample_rate: f32,
}

/// How long a click lasts, and how loud it starts.
const CLICK_SECONDS: f32 = 0.03;
const CLICK_LEVEL: f32 = 0.25;

impl Click {
    fn new(tempo: Tempo, beats_per_bar: u32) -> Self {
        Self {
            tempo,
            beats_per_bar: beats_per_bar.max(1),
            sample_rate: 48_000.0,
        }
    }

    fn prepare(&mut self, sample_rate: f32) {
        self.sample_rate = sample_rate;
    }

    fn add(&self, out: &mut [f32], channels: usize, start: u64) {
        let rate = f64::from(self.sample_rate);
        let beat_frames = self.tempo.seconds_per_beat() * rate;
        let length = (CLICK_SECONDS * self.sample_rate) as u64;
        for (i, frame) in out.chunks_exact_mut(channels).enumerate() {
            let at = start + i as u64;
            // The beat this frame belongs to, and how far into it.
            let beat = (at as f64 / beat_frames).floor() as u64;
            let since = at - (beat as f64 * beat_frames).round() as u64;
            if since >= length {
                continue;
            }
            let pitch = if beat.is_multiple_of(u64::from(self.beats_per_bar)) {
                1_760.0
            } else {
                880.0
            };
            let t = since as f64 / rate;
            let decay = 1.0 - since as f32 / length as f32;
            let s = CLICK_LEVEL * decay * (std::f64::consts::TAU * pitch * t).sin() as f32;
            for sample in frame {
                *sample += s;
            }
        }
    }
}

/// A performance as it was played: events with the seconds they came at.
/// [`finish`](Self::finish) makes it whole, so playing it back leaves no note
/// sounding and no pedal down.
#[derive(Default)]
pub struct Recording {
    events: Vec<(f64, Event)>,
    sounding: Vec<(u8, u8)>,
    pedal_down: [bool; 16],
}

impl Recording {
    pub fn add(&mut self, seconds: f64, event: Event) {
        match event {
            Event::NoteOn { channel, key, .. } => self.sounding.push((channel, key)),
            Event::NoteOff { channel, key, .. } => self.sounding.retain(|&n| n != (channel, key)),
            Event::Controller {
                channel,
                number: SUSTAIN,
                value,
            } => {
                self.pedal_down[usize::from(channel & 0x0f)] = value >= 64;
            }
            _ => {}
        }
        self.events.push((seconds.max(0.0), event));
    }

    pub fn is_empty(&self) -> bool {
        self.events.is_empty()
    }

    /// Ends the recording at `seconds`: notes still held are released and
    /// pedals still down come up, there. Events come out in order of time.
    pub fn finish(mut self, seconds: f64) -> Vec<(f64, Event)> {
        let end = self.events.iter().map(|e| e.0).fold(seconds, f64::max);
        for (channel, key) in std::mem::take(&mut self.sounding) {
            self.events.push((
                end,
                Event::NoteOff {
                    channel,
                    key,
                    velocity: 0.5,
                },
            ));
        }
        for (channel, down) in self.pedal_down.iter().enumerate() {
            if *down {
                let channel = channel as u8;
                self.events.push((
                    end,
                    Event::Controller {
                        channel,
                        number: SUSTAIN,
                        value: 0,
                    },
                ));
            }
        }
        self.events.sort_by(|a, b| a.0.total_cmp(&b.0));
        self.events
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::render_offline;

    const RATE: f32 = 48_000.0;

    fn on(key: u8) -> Event {
        Event::NoteOn {
            channel: 0,
            key,
            velocity: 1.0,
        }
    }

    fn off(key: u8) -> Event {
        Event::NoteOff {
            channel: 0,
            key,
            velocity: 0.5,
        }
    }

    #[test]
    fn converts_between_ticks_seconds_and_frames() {
        let tempo = Tempo::new(120.0);
        assert_eq!(tempo.seconds_at(TICKS_PER_BEAT), 0.5);
        assert_eq!(tempo.ticks_at(0.2), 384);
        assert_eq!(tempo.frame_at(384, RATE), 9_600);
        assert_eq!(tempo.ticks_at(-1.0), 0);
        // Recorded times snap to the nearest tick.
        assert_eq!(tempo.ticks_at(0.2003), 385);
    }

    /// Records the events it is given, with frames counted from the start.
    struct Recorder {
        seen: Vec<TimedEvent>,
        elapsed: u32,
    }

    impl Processor for Recorder {
        fn prepare(&mut self, _: f32, _: usize) {
            self.seen.clear();
            self.elapsed = 0;
        }

        fn process(&mut self, out: &mut [f32], channels: usize, events: &[TimedEvent]) {
            for e in events {
                self.seen.push(TimedEvent {
                    frame: e.frame + self.elapsed,
                    ..*e
                });
            }
            self.elapsed += (out.len() / channels) as u32;
        }
    }

    #[test]
    fn sequenced_events_land_on_their_frames_for_any_block_size() {
        let sequence = vec![(500, off(60)), (0, on(60)), (128, on(64)), (999, off(64))];
        let expected: Vec<TimedEvent> =
            [(0, on(60)), (128, on(64)), (500, off(60)), (999, off(64))]
                .into_iter()
                .map(|(frame, event)| TimedEvent { frame, event })
                .collect();
        for block in [1, 64, 128, 1_000] {
            let mut s = Sequencer::new(
                Recorder {
                    seen: Vec::new(),
                    elapsed: 0,
                },
                sequence.clone(),
            );
            render_offline(&mut s, RATE, 2, 1_000, block, &[]);
            assert_eq!(s.inner.seen, expected, "block size {block}");
        }
    }

    #[test]
    fn live_events_merge_with_the_sequence_in_order() {
        let sequence = vec![(10, on(60)), (30, off(60))];
        let live = [
            TimedEvent {
                frame: 5,
                event: on(72),
            },
            TimedEvent {
                frame: 10,
                event: off(72),
            },
            TimedEvent {
                frame: 40,
                event: on(74),
            },
        ];
        let mut s = Sequencer::new(
            Recorder {
                seen: Vec::new(),
                elapsed: 0,
            },
            sequence,
        );
        render_offline(&mut s, RATE, 1, 100, 100, &live);
        let order: Vec<(u32, Event)> = s.inner.seen.iter().map(|e| (e.frame, e.event)).collect();
        assert_eq!(
            order,
            [
                (5, on(72)),
                (10, on(60)),
                (10, off(72)),
                (30, off(60)),
                (40, on(74))
            ]
        );
    }

    #[test]
    fn prepare_starts_the_sequence_over() {
        let mut s = Sequencer::new(
            Recorder {
                seen: Vec::new(),
                elapsed: 0,
            },
            vec![(10, on(60))],
        );
        render_offline(&mut s, RATE, 1, 100, 32, &[]);
        render_offline(&mut s, RATE, 1, 100, 32, &[]);
        assert_eq!(s.inner.seen.len(), 1);
        assert_eq!(s.playhead(), 100);
    }

    struct Silence;

    impl Processor for Silence {
        fn prepare(&mut self, _: f32, _: usize) {}
        fn process(&mut self, out: &mut [f32], _: usize, _: &[TimedEvent]) {
            out.fill(0.0);
        }
    }

    #[test]
    fn the_click_sounds_on_every_beat_and_nowhere_else() {
        let tempo = Tempo::new(120.0);
        let mut s = Sequencer::new(Silence, Vec::new()).with_click(tempo, 4);
        let out = render_offline(&mut s, RATE, 1, 96_000, 128, &[]);
        let length = (CLICK_SECONDS * RATE) as usize;
        for beat in 0..4 {
            let start = beat * 24_000;
            assert!(
                out[start + 1..start + length]
                    .iter()
                    .any(|&x| x.abs() > 0.1),
                "beat {beat}"
            );
            assert!(
                out[start + length..start + 24_000]
                    .iter()
                    .all(|&x| x == 0.0),
                "after beat {beat}"
            );
        }
        // Every block size gives the same clicks.
        let mut again = Sequencer::new(Silence, Vec::new()).with_click(tempo, 4);
        assert_eq!(render_offline(&mut again, RATE, 1, 96_000, 1_000, &[]), out);
    }

    #[test]
    fn finishing_a_recording_releases_what_is_still_held() {
        let mut r = Recording::default();
        r.add(
            0.5,
            Event::Controller {
                channel: 0,
                number: SUSTAIN,
                value: 127,
            },
        );
        r.add(0.1, on(60));
        r.add(0.2, on(64));
        r.add(0.3, off(60));
        let events = r.finish(2.0);
        let times: Vec<f64> = events.iter().map(|e| e.0).collect();
        assert!(times.is_sorted());
        assert_eq!(events.last().unwrap().0, 2.0);
        assert!(events.contains(&(2.0, off(64))));
        assert!(events.contains(&(
            2.0,
            Event::Controller {
                channel: 0,
                number: SUSTAIN,
                value: 0
            }
        )));
        assert!(!events.iter().any(|e| e.0 == 2.0 && e.1 == off(60)));
    }

    #[test]
    fn a_recording_ends_no_earlier_than_its_last_event() {
        let mut r = Recording::default();
        r.add(3.0, on(60));
        let events = r.finish(1.0);
        assert_eq!(events, [(3.0, on(60)), (3.0, off(60))]);
    }
}
