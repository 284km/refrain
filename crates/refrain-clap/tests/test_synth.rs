//! Hosts refrain-test-synth in-process and checks what comes out.
//!
//! The synth is deterministic, so the host can be held to exact answers:
//! silence before the note, the right pitch and level while it sounds,
//! silence after its fade, and the same samples for any block size.

use clack_host::prelude::PluginEntry;
use refrain_clap::{ClapError, ClapPlugin, plugins_in};
use refrain_engine::{NoteEvent, TimedEvent, render_offline};
use refrain_test_synth::{FADE_SECONDS, ID, PEAK, RIGHT_GAIN};

const RATE: f32 = 48_000.0;
const NOTE_ON: u32 = 4_800; // 0.1 s
const NOTE_OFF: u32 = 28_800; // 0.6 s
const FRAMES: usize = 38_400; // 0.8 s

fn entry() -> PluginEntry {
    PluginEntry::load_from_clack::<refrain_test_synth::Entry>(c"refrain-test-synth").unwrap()
}

fn a4(velocity: f32) -> [TimedEvent; 2] {
    [
        TimedEvent {
            frame: NOTE_ON,
            event: NoteEvent::On {
                channel: 0,
                key: 69,
                velocity,
            },
        },
        TimedEvent {
            frame: NOTE_OFF,
            event: NoteEvent::Off {
                channel: 0,
                key: 69,
                velocity,
            },
        },
    ]
}

/// Renders stereo through a freshly activated plugin.
fn render(block: usize, events: &[TimedEvent]) -> Vec<f32> {
    render_to(2, block, events)
}

/// Renders to a device with `channels` channels.
fn render_to(channels: usize, block: usize, events: &[TimedEvent]) -> Vec<f32> {
    let entry = entry();
    let mut plugin = ClapPlugin::new(&entry, None).unwrap();
    let mut processor = plugin.activate(RATE, 1_024).unwrap();
    let out = render_offline(&mut processor, RATE, channels, FRAMES, block, events);
    assert!(!processor.has_failed());
    drop(processor);
    drop(plugin);
    out
}

fn left(out: &[f32]) -> Vec<f32> {
    out.iter().step_by(2).copied().collect()
}

#[test]
fn lists_the_plugin() {
    let plugins = plugins_in(&entry()).unwrap();
    assert_eq!(plugins.len(), 1);
    assert_eq!(plugins[0].id, ID);
    assert_eq!(plugins[0].name, "refrain test synth");
}

#[test]
fn an_unknown_id_is_an_error() {
    let err = ClapPlugin::new(&entry(), Some("no.such.plugin"))
        .err()
        .unwrap();
    assert!(matches!(err, ClapError::NotFound(_)), "{err}");
}

#[test]
fn a_note_sounds_at_its_pitch_and_level_then_stops() {
    let out = render(128, &a4(1.0));
    let l = left(&out);
    let fade = (FADE_SECONDS * RATE) as usize;

    // Silence until the note starts, exactly.
    assert!(l[..NOTE_ON as usize].iter().all(|&s| s == 0.0));

    // While it sounds: 440 Hz and the full level.
    let steady = &l[NOTE_ON as usize + fade..NOTE_OFF as usize];
    let rising = steady
        .windows(2)
        .filter(|w| w[0] < 0.0 && w[1] >= 0.0)
        .count();
    let seconds = steady.len() as f64 / f64::from(RATE);
    let expected = 440.0 * seconds;
    assert!(
        (rising as f64 - expected).abs() <= 1.0,
        "{rising} cycles, expected {expected}"
    );
    let peak = steady.iter().fold(0.0_f32, |m, s| m.max(s.abs()));
    assert!((peak - PEAK).abs() < 1e-3, "peak {peak}");

    // Silence once the fade out is over.
    let after = NOTE_OFF as usize + fade + 1;
    assert!(l[after..].iter().all(|&s| s == 0.0));

    // The right channel is the left one, scaled. A host that swaps or
    // duplicates channels fails here.
    for frame in out.as_chunks::<2>().0 {
        assert_eq!(frame[1], frame[0] * RIGHT_GAIN);
    }
}

#[test]
fn velocity_scales_the_level() {
    let l = left(&render(128, &a4(0.5)));
    let fade = (FADE_SECONDS * RATE) as usize;
    let steady = &l[NOTE_ON as usize + fade..NOTE_OFF as usize];
    let peak = steady.iter().fold(0.0_f32, |m, s| m.max(s.abs()));
    assert!((peak - PEAK * 0.5).abs() < 1e-3, "peak {peak}");
}

#[test]
fn output_does_not_depend_on_block_size() {
    // Note on and off land in the middle of blocks for most of these sizes,
    // so this also checks that event frames reach the plugin correctly.
    let reference = render(1_024, &a4(1.0));
    for block in [1, 64, 100, 128, 1_000] {
        assert_eq!(render(block, &a4(1.0)), reference, "block size {block}");
    }
}

#[test]
fn chords_add_up() {
    let chord: Vec<TimedEvent> = [60, 64, 67]
        .into_iter()
        .flat_map(|key| {
            [
                TimedEvent {
                    frame: NOTE_ON,
                    event: NoteEvent::On {
                        channel: 0,
                        key,
                        velocity: 1.0,
                    },
                },
                TimedEvent {
                    frame: NOTE_OFF,
                    event: NoteEvent::Off {
                        channel: 0,
                        key,
                        velocity: 1.0,
                    },
                },
            ]
        })
        .collect::<Vec<_>>();
    let mut sorted = chord.clone();
    sorted.sort_by_key(|e| e.frame);
    let together = left(&render(128, &sorted));

    let mut sum = vec![0.0_f32; FRAMES];
    for pair in chord.chunks(2) {
        for (s, x) in sum.iter_mut().zip(left(&render(128, pair))) {
            *s += x;
        }
    }
    for (i, (a, b)) in together.iter().zip(&sum).enumerate() {
        assert!((a - b).abs() < 1e-6, "frame {i}: {a} vs {b}");
    }
}

#[test]
fn stereo_is_mixed_down_for_mono_and_padded_for_more_channels() {
    let stereo = render(128, &a4(1.0));
    let mono = render_to(1, 128, &a4(1.0));
    for (m, lr) in mono.iter().zip(stereo.as_chunks::<2>().0) {
        assert_eq!(*m, (lr[0] + lr[1]) / 2.0);
    }
    let quad = render_to(4, 128, &a4(1.0));
    for (q, lr) in quad
        .as_chunks::<4>()
        .0
        .iter()
        .zip(stereo.as_chunks::<2>().0)
    {
        assert_eq!(q, &[lr[0], lr[1], 0.0, 0.0]);
    }
}
