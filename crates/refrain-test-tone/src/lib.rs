//! The sound of refrain's test plugins.
//!
//! Each note is a sine wave at the note's pitch, with an amplitude of
//! `velocity * PEAK` and a linear 5 ms fade in and out. [`Tone::write_right`]
//! makes the right channel the left one times [`RIGHT_GAIN`], so a host that
//! swaps or duplicates channels shows up in tests. Everything is
//! deterministic, so a host can check what it hears against what it sent.
//!
//! The CLAP and VST3 test plugins both play this, so the same performance
//! must give the same samples through either plugin format.

/// Amplitude of a note at full velocity.
pub const PEAK: f32 = 0.2;

/// The right channel is the left channel times this. A power of two, so the
/// product is exact.
pub const RIGHT_GAIN: f32 = 0.5;

/// Length of the fade in and the fade out.
pub const FADE_SECONDS: f32 = 0.005;

/// How far a full pitch bend goes, in semitones.
pub const BEND_SEMITONES: f64 = 2.0;

const VOICES: usize = 16;

/// The pitch of a MIDI key, with A4 (key 69) at 440 Hz.
pub fn key_frequency(key: u8) -> f64 {
    440.0 * 2f64.powf((f64::from(key) - 69.0) / 12.0)
}

#[derive(Clone, Copy, Default)]
struct Voice {
    active: bool,
    channel: u16,
    key: u16,
    // Kept in [0, 1) and in f64, so the pitch does not drift.
    phase: f64,
    // Phase step at the key's own pitch, before pitch bend.
    base_step: f64,
    gain: f32,
    level: f32,
    releasing: bool,
    // Released by the player while the pedal was down.
    held: bool,
}

/// A small polyphonic sine synth. `None` for a channel or key means any.
pub struct Tone {
    voices: [Voice; VOICES],
    sample_rate: f64,
    fade_step: f32,
    pedal_down: [bool; 16],
    // Pitch factor per MIDI channel, from pitch bend.
    bend: [f64; 16],
}

fn matches(want: Option<u16>, have: u16) -> bool {
    want.is_none_or(|w| w == have)
}

impl Tone {
    pub fn new(sample_rate: f64) -> Self {
        Self {
            voices: [Voice::default(); VOICES],
            sample_rate,
            fade_step: 1.0 / (FADE_SECONDS * sample_rate as f32),
            pedal_down: [false; 16],
            bend: [1.0; 16],
        }
    }

    pub fn note_on(&mut self, channel: u16, key: u16, velocity: f32) {
        // Striking a key the pedal holds releases the old voice. Without the
        // pedal, a repeated note simply adds a voice, so a host that forgets
        // to release held notes is heard.
        self.release(|v| v.channel == channel && v.key == key && v.held);
        // Reuse a free voice. When all are busy, the new note is dropped.
        let Some(voice) = self.voices.iter_mut().find(|v| !v.active) else {
            return;
        };
        *voice = Voice {
            active: true,
            channel,
            key,
            phase: 0.0,
            base_step: key_frequency(key.min(127) as u8) / self.sample_rate,
            gain: velocity * PEAK,
            level: 0.0,
            releasing: false,
            held: false,
        };
    }

    pub fn note_off(&mut self, channel: Option<u16>, key: Option<u16>) {
        let pedal = self.pedal_down;
        for v in self.voices.iter_mut().filter(|v| v.active && !v.releasing) {
            if matches(channel, v.channel) && matches(key, v.key) {
                if pedal[usize::from(v.channel & 0x0f)] {
                    v.held = true;
                } else {
                    v.releasing = true;
                }
            }
        }
    }

    /// Silences matching voices at once, without a fade.
    pub fn choke(&mut self, channel: Option<u16>, key: Option<u16>) {
        for v in self.voices.iter_mut() {
            if matches(channel, v.channel) && matches(key, v.key) {
                v.active = false;
            }
        }
    }

    /// The sustain pedal on `channel`. Lifting it releases the notes it held.
    pub fn pedal(&mut self, channel: u16, down: bool) {
        let c = usize::from(channel & 0x0f);
        if self.pedal_down[c] && !down {
            self.release(|v| v.channel == channel && v.held);
        }
        self.pedal_down[c] = down;
    }

    /// Releases every note on `channel`, held ones too.
    pub fn all_notes_off(&mut self, channel: u16) {
        self.release(|v| v.channel == channel);
    }

    /// Pitch bend on `channel`, as a 14-bit MIDI value centred on 8192.
    pub fn bend(&mut self, channel: u16, value: u16) {
        let semitones = (f64::from(value) - 8192.0) / 8192.0 * BEND_SEMITONES;
        self.bend[usize::from(channel & 0x0f)] = 2f64.powf(semitones / 12.0);
    }

    /// Plays a MIDI 1.0 message: notes, the sustain pedal, All Notes Off and
    /// pitch bend. Anything else is ignored.
    pub fn midi(&mut self, [status, a, b]: [u8; 3]) {
        let channel = u16::from(status & 0x0f);
        match (status & 0xf0, a) {
            (0x90, _) if b > 0 => self.note_on(channel, a.into(), f32::from(b) / 127.0),
            (0x80 | 0x90, _) => self.note_off(Some(channel), Some(a.into())),
            (0xb0, 64) => self.pedal(channel, b >= 64),
            (0xb0, 123) => self.all_notes_off(channel),
            (0xe0, _) => self.bend(channel, u16::from(b) << 7 | u16::from(a)),
            _ => {}
        }
    }

    fn release(&mut self, pick: impl Fn(&Voice) -> bool) {
        for v in self.voices.iter_mut().filter(|v| v.active && pick(v)) {
            v.releasing = true;
            v.held = false;
        }
    }

    /// Adds the sound of the next `out.len()` samples to `out`.
    pub fn render(&mut self, out: &mut [f32]) {
        let bend = self.bend;
        for v in self.voices.iter_mut().filter(|v| v.active) {
            let step = v.base_step * bend[usize::from(v.channel & 0x0f)];
            for sample in out.iter_mut() {
                if v.releasing {
                    v.level -= self.fade_step;
                    if v.level <= 0.0 {
                        v.active = false;
                        break;
                    }
                } else if v.level < 1.0 {
                    v.level = (v.level + self.fade_step).min(1.0);
                }
                let s = (std::f64::consts::TAU * v.phase).sin() as f32;
                *sample += s * v.gain * v.level;
                v.phase += step;
                v.phase -= v.phase.floor();
            }
        }
    }

    /// Writes the right channel from the left one.
    pub fn write_right(left: &[f32], right: &mut [f32]) {
        for (r, &l) in right.iter_mut().zip(left) {
            *r = l * RIGHT_GAIN;
        }
    }

    pub fn is_sounding(&self) -> bool {
        self.voices.iter().any(|v| v.active)
    }

    pub fn stop(&mut self) {
        self.choke(None, None);
    }
}
