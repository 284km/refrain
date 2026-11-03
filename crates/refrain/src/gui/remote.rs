//! Driving a mixer on the audio thread from the GUI.
//!
//! The GUI sends commands through a lock-free queue; the audio thread applies
//! them before its next block and publishes where the transport is. Nothing
//! here allocates or locks on the audio thread.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use refrain_engine::{Mix, Mixer, Processor, Sequence, TimedEvent};

/// What the GUI asks of the mixer.
#[derive(Clone, Debug, PartialEq)]
pub enum Command {
    Play,
    Stop,
    /// Move the playhead to this frame.
    Locate(u64),
    Mix(usize, Mix),
    MasterVolume(f32),
    Click(bool),
    LiveTrack(usize),
    /// Play this sequence on a track from now on.
    Sequence(usize, Sequence),
}

/// Where the transport is, as the audio thread last saw it.
#[derive(Default)]
pub struct Status {
    playhead: AtomicU64,
    playing: AtomicBool,
    failed: AtomicBool,
}

impl Status {
    pub fn playhead(&self) -> u64 {
        self.playhead.load(Ordering::Acquire)
    }

    pub fn playing(&self) -> bool {
        self.playing.load(Ordering::Acquire)
    }

    /// Whether a plugin has failed and gives silence.
    pub fn failed(&self) -> bool {
        self.failed.load(Ordering::Acquire)
    }
}

/// A mixer that takes commands. It is the processor the audio stream runs.
pub struct Remote {
    mixer: Mixer,
    commands: rtrb::Consumer<Command>,
    // Sequences that were replaced go back, to be freed off the audio thread.
    spent: rtrb::Producer<Sequence>,
    status: Arc<Status>,
}

impl Remote {
    pub fn new(
        mixer: Mixer,
        commands: rtrb::Consumer<Command>,
        spent: rtrb::Producer<Sequence>,
        status: Arc<Status>,
    ) -> Self {
        Self {
            mixer,
            commands,
            spent,
            status,
        }
    }

    fn apply(&mut self, command: Command) {
        let m = &mut self.mixer;
        match command {
            Command::Play => m.play(),
            Command::Stop => m.stop(),
            Command::Locate(frame) => m.locate(frame),
            Command::Mix(track, mix) => m.set_mix(track, mix),
            Command::MasterVolume(db) => m.set_master_volume(db),
            Command::Click(on) => m.set_click(on),
            Command::LiveTrack(track) => m.set_live_track(track),
            Command::Sequence(track, sequence) => {
                let old = m.set_sequence(track, sequence).unwrap_or_else(|new| new);
                // With the queue full, the old one is freed here after all.
                let _ = self.spent.push(old);
            }
        }
    }
}

impl Processor for Remote {
    fn prepare(&mut self, sample_rate: f32, max_block_frames: usize) {
        self.mixer.prepare(sample_rate, max_block_frames);
    }

    fn process(&mut self, out: &mut [f32], channels: usize, events: &[TimedEvent]) {
        while let Ok(command) = self.commands.pop() {
            self.apply(command);
        }
        self.mixer.process(out, channels, events);
        let s = &self.status;
        s.playhead.store(self.mixer.playhead(), Ordering::Release);
        s.playing.store(self.mixer.is_playing(), Ordering::Release);
        s.failed.store(self.mixer.has_failed(), Ordering::Release);
    }

    fn has_failed(&self) -> bool {
        self.mixer.has_failed()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use refrain_engine::{Event, Sine, Track};

    #[test]
    fn commands_take_effect_at_the_next_block() {
        let mixer = Mixer::new(
            vec![Track::new(
                vec![Box::new(Sine::new(440.0, 0.5))],
                Mix::default(),
            )],
            Vec::new(),
            0.0,
        )
        .stopped();
        let (mut to_audio, commands) = rtrb::RingBuffer::new(16);
        let (spent, mut freed) = rtrb::RingBuffer::new(4);
        let status = Arc::new(Status::default());
        let mut remote = Remote::new(mixer, commands, spent, Arc::clone(&status));
        remote.prepare(48_000.0, 128);
        let mut out = vec![0.0; 256];
        remote.process(&mut out, 2, &[]);
        assert_eq!((status.playhead(), status.playing()), (0, false));

        to_audio.push(Command::Locate(1_000)).unwrap();
        to_audio.push(Command::Play).unwrap();
        remote.process(&mut out, 2, &[]);
        assert_eq!((status.playhead(), status.playing()), (1_128, true));

        let muted = Mix {
            mute: true,
            ..Mix::default()
        };
        to_audio.push(Command::Mix(0, muted)).unwrap();
        to_audio.push(Command::Stop).unwrap();
        remote.process(&mut out, 2, &[]);
        assert!(out.iter().all(|&s| s == 0.0));
        assert_eq!((status.playhead(), status.playing()), (1_128, false));

        // A replaced sequence comes back to be freed.
        let note = Event::NoteOn {
            channel: 0,
            key: 60,
            velocity: 1.0,
        };
        to_audio
            .push(Command::Sequence(0, vec![(2_000, note)]))
            .unwrap();
        remote.process(&mut out, 2, &[]);
        assert_eq!(freed.pop(), Ok(Vec::new()));
        to_audio.push(Command::Sequence(0, Vec::new())).unwrap();
        remote.process(&mut out, 2, &[]);
        assert_eq!(freed.pop(), Ok(vec![(2_000, note)]));
    }
}
