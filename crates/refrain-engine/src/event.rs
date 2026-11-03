//! Performance events: notes, controllers and pitch bend.

/// The MIDI controller number of the sustain (damper) pedal.
pub const SUSTAIN: u8 = 64;

/// The MIDI controller number that releases every note on a channel.
pub const ALL_NOTES_OFF: u8 = 123;

/// The centre of the 14-bit pitch bend range: no bend.
pub const PITCH_BEND_CENTRE: u16 = 8192;

/// Something a player does. Values keep MIDI 1.0's ranges, so any event can
/// be passed on to a plugin that takes MIDI without losing anything.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Event {
    /// `velocity` is in `0.0..=1.0`.
    NoteOn { channel: u8, key: u8, velocity: f32 },
    /// `velocity` is the release velocity, in `0.0..=1.0`.
    NoteOff { channel: u8, key: u8, velocity: f32 },
    /// A control change, such as the sustain pedal. `value` is `0..=127`.
    Controller { channel: u8, number: u8, value: u8 },
    /// `value` is `0..=16383`, with [`PITCH_BEND_CENTRE`] meaning no bend.
    PitchBend { channel: u8, value: u16 },
    /// Aftertouch for the whole channel. `value` is `0..=127`.
    ChannelPressure { channel: u8, value: u8 },
    /// Aftertouch for one key. `value` is `0..=127`.
    PolyPressure { channel: u8, key: u8, value: u8 },
}

impl Event {
    /// Reads a MIDI 1.0 channel voice message. Returns `None` for anything
    /// else, and for malformed messages.
    pub fn from_midi(bytes: &[u8]) -> Option<Self> {
        let (&status, data) = bytes.split_first()?;
        if data.iter().any(|&b| b > 127) {
            return None;
        }
        let channel = status & 0x0f;
        let one = || data.first().copied();
        let two = || match data {
            [a, b, ..] => Some((*a, *b)),
            _ => None,
        };
        Some(match status & 0xf0 {
            0x80 => {
                let (key, v) = two()?;
                Self::NoteOff {
                    channel,
                    key,
                    velocity: f32::from(v) / 127.0,
                }
            }
            // A note on with velocity 0 is a note off, by convention.
            0x90 => match two()? {
                (key, 0) => Self::NoteOff {
                    channel,
                    key,
                    velocity: 0.5,
                },
                (key, v) => Self::NoteOn {
                    channel,
                    key,
                    velocity: f32::from(v) / 127.0,
                },
            },
            0xa0 => {
                let (key, value) = two()?;
                Self::PolyPressure {
                    channel,
                    key,
                    value,
                }
            }
            0xb0 => {
                let (number, value) = two()?;
                Self::Controller {
                    channel,
                    number,
                    value,
                }
            }
            0xd0 => Self::ChannelPressure {
                channel,
                value: one()?,
            },
            0xe0 => {
                let (lsb, msb) = two()?;
                Self::PitchBend {
                    channel,
                    value: u16::from(msb) << 7 | u16::from(lsb),
                }
            }
            _ => return None,
        })
    }

    /// Writes the event as a MIDI 1.0 message. Two-byte messages are padded
    /// with a zero.
    pub fn to_midi(&self) -> [u8; 3] {
        let velocity = |v: f32| (v.clamp(0.0, 1.0) * 127.0).round() as u8;
        match *self {
            Self::NoteOn {
                channel,
                key,
                velocity: v,
            } => [0x90 | channel, key, velocity(v).max(1)],
            Self::NoteOff {
                channel,
                key,
                velocity: v,
            } => [0x80 | channel, key, velocity(v)],
            Self::PolyPressure {
                channel,
                key,
                value,
            } => [0xa0 | channel, key, value],
            Self::Controller {
                channel,
                number,
                value,
            } => [0xb0 | channel, number, value],
            Self::ChannelPressure { channel, value } => [0xd0 | channel, value, 0],
            Self::PitchBend { channel, value } => {
                [0xe0 | channel, (value & 0x7f) as u8, (value >> 7) as u8]
            }
        }
    }

    pub fn is_note(&self) -> bool {
        matches!(self, Self::NoteOn { .. } | Self::NoteOff { .. })
    }
}

/// An event at a frame offset. Inside a block, `frame` counts from the start
/// of the block. Events in a block are in order of `frame`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TimedEvent {
    pub frame: u32,
    pub event: Event,
}

/// The most note offs one block of input can turn into: every key on every
/// channel, released by the pedal at once.
pub const MAX_PEDAL_RELEASES: usize = 16 * 128;

/// Plays the sustain pedal for an instrument that only understands notes.
///
/// While the pedal is down on a channel, note offs on it are held back. When
/// the pedal comes up, the held notes are released. Striking a held key again
/// releases it first, so voices do not pile up. All Notes Off releases every
/// note on its channel. Every other controller, and pitch bend and pressure,
/// is dropped: a notes-only instrument has no way to take them.
pub struct SustainPedal {
    down: [bool; 16],
    held: [[bool; 128]; 16],
    sounding: [[bool; 128]; 16],
}

impl Default for SustainPedal {
    fn default() -> Self {
        Self {
            down: [false; 16],
            held: [[false; 128]; 16],
            sounding: [[false; 128]; 16],
        }
    }
}

impl SustainPedal {
    pub fn reset(&mut self) {
        *self = Self::default();
    }

    /// Turns `input` into notes and appends them to `out`, in order.
    ///
    /// `out` should have room for `input.len() + MAX_PEDAL_RELEASES` more
    /// events; then this never allocates.
    pub fn apply(&mut self, input: &[TimedEvent], out: &mut Vec<TimedEvent>) {
        for e in input {
            let at = |event| TimedEvent {
                frame: e.frame,
                event,
            };
            match e.event {
                Event::NoteOn { channel, key, .. } => {
                    let (c, k) = (usize::from(channel & 0x0f), usize::from(key & 0x7f));
                    if self.held[c][k] {
                        self.held[c][k] = false;
                        out.push(at(Event::NoteOff {
                            channel,
                            key,
                            velocity: 0.5,
                        }));
                    }
                    self.sounding[c][k] = true;
                    out.push(*e);
                }
                Event::NoteOff { channel, key, .. } => {
                    let (c, k) = (usize::from(channel & 0x0f), usize::from(key & 0x7f));
                    if !self.sounding[c][k] {
                        // Not ours to release; pass it on unchanged.
                        out.push(*e);
                    } else if self.down[c] {
                        self.held[c][k] = true;
                    } else {
                        self.sounding[c][k] = false;
                        out.push(*e);
                    }
                }
                Event::Controller {
                    channel,
                    number: SUSTAIN,
                    value,
                } => {
                    let c = usize::from(channel & 0x0f);
                    let down = value >= 64;
                    if self.down[c] && !down {
                        self.release(channel, self.held[c], e.frame, out);
                    }
                    self.down[c] = down;
                }
                Event::Controller {
                    channel,
                    number: ALL_NOTES_OFF,
                    ..
                } => {
                    let c = usize::from(channel & 0x0f);
                    self.release(channel, self.sounding[c], e.frame, out);
                }
                _ => {}
            }
        }
    }

    /// Releases the keys on `channel` that are set in `keys`.
    fn release(&mut self, channel: u8, keys: [bool; 128], frame: u32, out: &mut Vec<TimedEvent>) {
        let c = usize::from(channel & 0x0f);
        for (k, _) in keys.iter().enumerate().filter(|(_, picked)| **picked) {
            self.held[c][k] = false;
            self.sounding[c][k] = false;
            out.push(TimedEvent {
                frame,
                event: Event::NoteOff {
                    channel,
                    key: k as u8,
                    velocity: 0.5,
                },
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn on(frame: u32, key: u8) -> TimedEvent {
        TimedEvent {
            frame,
            event: Event::NoteOn {
                channel: 0,
                key,
                velocity: 1.0,
            },
        }
    }
    fn off(frame: u32, key: u8) -> TimedEvent {
        TimedEvent {
            frame,
            event: Event::NoteOff {
                channel: 0,
                key,
                velocity: 0.5,
            },
        }
    }
    fn cc(frame: u32, number: u8, value: u8) -> TimedEvent {
        TimedEvent {
            frame,
            event: Event::Controller {
                channel: 0,
                number,
                value,
            },
        }
    }

    fn pedal(input: &[TimedEvent]) -> Vec<TimedEvent> {
        let mut out = Vec::new();
        SustainPedal::default().apply(input, &mut out);
        out
    }

    #[test]
    fn reads_and_writes_midi() {
        let cases: [(&[u8], Event); 7] = [
            (
                &[0x91, 60, 127],
                Event::NoteOn {
                    channel: 1,
                    key: 60,
                    velocity: 1.0,
                },
            ),
            (
                &[0x80, 60, 0],
                Event::NoteOff {
                    channel: 0,
                    key: 60,
                    velocity: 0.0,
                },
            ),
            (
                &[0xb0, 64, 127],
                Event::Controller {
                    channel: 0,
                    number: 64,
                    value: 127,
                },
            ),
            (
                &[0xe2, 0x00, 0x40],
                Event::PitchBend {
                    channel: 2,
                    value: 8192,
                },
            ),
            (
                &[0xe0, 0x7f, 0x7f],
                Event::PitchBend {
                    channel: 0,
                    value: 16383,
                },
            ),
            (
                &[0xd3, 90],
                Event::ChannelPressure {
                    channel: 3,
                    value: 90,
                },
            ),
            (
                &[0xa0, 60, 33],
                Event::PolyPressure {
                    channel: 0,
                    key: 60,
                    value: 33,
                },
            ),
        ];
        for (bytes, event) in cases {
            assert_eq!(Event::from_midi(bytes), Some(event), "{bytes:02x?}");
            let mut written = event.to_midi().to_vec();
            written.truncate(bytes.len());
            assert_eq!(written, bytes, "{event:?}");
        }
    }

    #[test]
    fn a_note_on_with_velocity_zero_is_a_note_off() {
        assert!(matches!(
            Event::from_midi(&[0x90, 60, 0]),
            Some(Event::NoteOff { key: 60, .. })
        ));
    }

    #[test]
    fn rejects_what_it_does_not_read() {
        assert_eq!(Event::from_midi(&[]), None);
        assert_eq!(Event::from_midi(&[0x90, 60]), None);
        assert_eq!(Event::from_midi(&[0x90, 200, 1]), None);
        assert_eq!(Event::from_midi(&[0xc0, 5]), None); // program change
        assert_eq!(Event::from_midi(&[0xf8]), None); // clock
    }

    #[test]
    fn without_the_pedal_notes_pass_through() {
        let input = [on(0, 60), off(10, 60)];
        assert_eq!(pedal(&input), input);
    }

    #[test]
    fn the_pedal_holds_note_offs_until_it_comes_up() {
        let out = pedal(&[
            cc(0, SUSTAIN, 127),
            on(5, 60),
            off(10, 60),
            cc(20, SUSTAIN, 0),
        ]);
        assert_eq!(out, [on(5, 60), off(20, 60)]);
    }

    #[test]
    fn the_pedal_is_down_from_64() {
        let held = pedal(&[cc(0, SUSTAIN, 64), on(5, 60), off(10, 60)]);
        assert_eq!(held, [on(5, 60)]);
        let not_held = pedal(&[cc(0, SUSTAIN, 63), on(5, 60), off(10, 60)]);
        assert_eq!(not_held, [on(5, 60), off(10, 60)]);
    }

    #[test]
    fn notes_released_before_the_pedal_are_not_held() {
        let out = pedal(&[
            on(0, 60),
            off(5, 60),
            cc(10, SUSTAIN, 127),
            cc(20, SUSTAIN, 0),
        ]);
        assert_eq!(out, [on(0, 60), off(5, 60)]);
    }

    #[test]
    fn keys_still_down_when_the_pedal_comes_up_keep_sounding() {
        let out = pedal(&[
            cc(0, SUSTAIN, 127),
            on(5, 60),
            cc(20, SUSTAIN, 0),
            off(30, 60),
        ]);
        assert_eq!(out, [on(5, 60), off(30, 60)]);
    }

    #[test]
    fn striking_a_held_key_again_releases_it_first() {
        let out = pedal(&[
            cc(0, SUSTAIN, 127),
            on(5, 60),
            off(10, 60),
            on(15, 60),
            cc(20, SUSTAIN, 0),
        ]);
        assert_eq!(out, [on(5, 60), off(15, 60), on(15, 60)]);
    }

    #[test]
    fn all_notes_off_releases_everything_on_the_channel() {
        let out = pedal(&[
            cc(0, SUSTAIN, 127),
            on(1, 60),
            off(2, 60),
            on(3, 64),
            cc(9, ALL_NOTES_OFF, 0),
        ]);
        assert_eq!(out, [on(1, 60), on(3, 64), off(9, 60), off(9, 64)]);
    }

    #[test]
    fn the_pedal_is_per_channel_and_other_controllers_are_dropped() {
        let other_channel = TimedEvent {
            frame: 10,
            event: Event::NoteOff {
                channel: 1,
                key: 60,
                velocity: 0.5,
            },
        };
        let other_on = TimedEvent {
            event: Event::NoteOn {
                channel: 1,
                key: 60,
                velocity: 1.0,
            },
            ..other_channel
        };
        let bend = TimedEvent {
            frame: 12,
            event: Event::PitchBend {
                channel: 0,
                value: 0,
            },
        };
        let out = pedal(&[
            cc(0, SUSTAIN, 127),
            other_on,
            other_channel,
            bend,
            cc(13, 1, 100),
        ]);
        assert_eq!(out, [other_on, other_channel]);
    }

    #[test]
    fn releasing_every_key_fits_the_promised_room() {
        let mut input = vec![cc(0, SUSTAIN, 127)];
        for key in 0..128 {
            input.push(on(1, key));
            input.push(off(2, key));
        }
        input.push(cc(3, SUSTAIN, 0));
        let mut out = Vec::with_capacity(input.len() + MAX_PEDAL_RELEASES);
        let capacity = out.capacity();
        SustainPedal::default().apply(&input, &mut out);
        assert_eq!(out.len(), 256);
        assert_eq!(out.capacity(), capacity, "apply allocated");
    }
}
