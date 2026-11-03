//! Hosts refrain-test-synth in-process and checks what comes out.
//!
//! The synth is deterministic, so the host can be held to exact answers:
//! silence before the note, the right pitch and level while it sounds,
//! silence after its fade, and the same samples for any block size.
//!
//! The file holds two plugins that sound the same. One plays the sustain
//! pedal itself from MIDI; for the other, which takes notes only, the host
//! plays it. Both must give the same samples.

use clack_host::prelude::PluginEntry;
use refrain_clap::{ClapError, ClapPlugin, plugins_in};
use refrain_engine::{
    ALL_NOTES_OFF, Event, PITCH_BEND_CENTRE, SUSTAIN, TimedEvent, render_offline,
};
use refrain_test_synth::{
    BEND_SEMITONES, FADE_SECONDS, ID, NOTES_ONLY_ID, PEAK, RIGHT_GAIN, key_frequency,
};

const RATE: f32 = 48_000.0;
const FRAMES: usize = 38_400; // 0.8 s

fn entry() -> PluginEntry {
    PluginEntry::load_from_clack::<refrain_test_synth::Entry>(c"refrain-test-synth").unwrap()
}

fn s(seconds: f64) -> u32 {
    (seconds * f64::from(RATE)) as u32
}

fn on(at: f64, key: u8, velocity: f32) -> TimedEvent {
    TimedEvent {
        frame: s(at),
        event: Event::NoteOn {
            channel: 0,
            key,
            velocity,
        },
    }
}

fn off(at: f64, key: u8) -> TimedEvent {
    TimedEvent {
        frame: s(at),
        event: Event::NoteOff {
            channel: 0,
            key,
            velocity: 0.5,
        },
    }
}

fn cc(at: f64, number: u8, value: u8) -> TimedEvent {
    TimedEvent {
        frame: s(at),
        event: Event::Controller {
            channel: 0,
            number,
            value,
        },
    }
}

fn bend(at: f64, value: u16) -> TimedEvent {
    TimedEvent {
        frame: s(at),
        event: Event::PitchBend { channel: 0, value },
    }
}

/// A4 from 0.1 s to 0.6 s.
fn a4(velocity: f32) -> [TimedEvent; 2] {
    [on(0.1, 69, velocity), off(0.6, 69)]
}

/// Renders through a freshly activated plugin, to a device with `channels`
/// channels.
fn render_to(id: &str, channels: usize, block: usize, events: &[TimedEvent]) -> Vec<f32> {
    let entry = entry();
    let mut plugin = ClapPlugin::new(&entry, Some(id)).unwrap();
    let mut processor = plugin.activate(RATE, 1_024).unwrap();
    let out = render_offline(&mut processor, RATE, channels, FRAMES, block, events);
    assert!(!processor.has_failed());
    drop(processor);
    drop(plugin);
    out
}

fn render(id: &str, block: usize, events: &[TimedEvent]) -> Vec<f32> {
    render_to(id, 2, block, events)
}

/// The left channel of a stereo render.
fn left(out: &[f32]) -> Vec<f32> {
    out.iter().step_by(2).copied().collect()
}

fn between(l: &[f32], from: f64, to: f64) -> &[f32] {
    &l[s(from) as usize..s(to) as usize]
}

fn peak(x: &[f32]) -> f32 {
    x.iter().fold(0.0_f32, |m, s| m.max(s.abs()))
}

fn frequency(x: &[f32]) -> f64 {
    let rising = x.windows(2).filter(|w| w[0] < 0.0 && w[1] >= 0.0).count();
    rising as f64 / (x.len() as f64 / f64::from(RATE))
}

fn silent(x: &[f32]) -> bool {
    x.iter().all(|&s| s == 0.0)
}

const FADE: f64 = FADE_SECONDS as f64;

#[test]
fn lists_both_plugins() {
    let ids: Vec<String> = plugins_in(&entry())
        .unwrap()
        .into_iter()
        .map(|p| p.id)
        .collect();
    assert_eq!(ids, [ID, NOTES_ONLY_ID]);
}

#[test]
fn choosing_a_plugin_needs_an_id_when_the_file_holds_several() {
    let err = ClapPlugin::new(&entry(), None).err().unwrap();
    assert!(matches!(err, ClapError::Ambiguous(_)), "{err}");
    let err = ClapPlugin::new(&entry(), Some("no.such.plugin"))
        .err()
        .unwrap();
    assert!(matches!(err, ClapError::NotFound(_)), "{err}");
}

#[test]
fn a_note_sounds_at_its_pitch_and_level_then_stops() {
    for id in [ID, NOTES_ONLY_ID] {
        let out = render(id, 128, &a4(1.0));
        let l = left(&out);
        assert!(silent(between(&l, 0.0, 0.1)), "{id}");
        let steady = between(&l, 0.1 + FADE, 0.6);
        let tolerance = 1.0 / (steady.len() as f64 / f64::from(RATE));
        assert!((frequency(steady) - 440.0).abs() <= tolerance, "{id}");
        assert!((peak(steady) - PEAK).abs() < 1e-3, "{id}");
        assert!(silent(between(&l, 0.6 + FADE + 0.001, 0.8)), "{id}");

        // The right channel is the left one, scaled. A host that swaps or
        // duplicates channels fails here.
        for frame in out.as_chunks::<2>().0 {
            assert_eq!(frame[1], frame[0] * RIGHT_GAIN);
        }
    }
}

#[test]
fn velocity_scales_the_level() {
    let l = left(&render(ID, 128, &a4(0.5)));
    assert!((peak(between(&l, 0.1 + FADE, 0.6)) - PEAK * 0.5).abs() < 1e-3);
}

#[test]
fn output_does_not_depend_on_block_size() {
    // Events land in the middle of blocks for most of these sizes, so this
    // also checks that their frames reach the plugin correctly.
    let events = [
        cc(0.05, SUSTAIN, 127),
        on(0.1, 69, 1.0),
        off(0.3, 69),
        bend(0.35, 12_000),
        cc(0.5, SUSTAIN, 0),
    ];
    for id in [ID, NOTES_ONLY_ID] {
        let reference = render(id, 1_024, &events);
        for block in [1, 64, 100, 128, 1_000] {
            assert_eq!(
                render(id, block, &events),
                reference,
                "{id}, block size {block}"
            );
        }
    }
}

#[test]
fn stereo_is_mixed_down_for_mono_and_padded_for_more_channels() {
    let stereo = render(ID, 128, &a4(1.0));
    let mono = render_to(ID, 1, 128, &a4(1.0));
    for (m, lr) in mono.iter().zip(stereo.as_chunks::<2>().0) {
        assert_eq!(*m, (lr[0] + lr[1]) / 2.0);
    }
    let quad = render_to(ID, 4, 128, &a4(1.0));
    for (q, lr) in quad
        .as_chunks::<4>()
        .0
        .iter()
        .zip(stereo.as_chunks::<2>().0)
    {
        assert_eq!(q, &[lr[0], lr[1], 0.0, 0.0]);
    }
}

#[test]
fn chords_add_up() {
    let notes = |key| [on(0.1, key, 1.0), off(0.6, key)];
    let mut chord: Vec<TimedEvent> = [60, 64, 67].into_iter().flat_map(notes).collect();
    chord.sort_by_key(|e| e.frame);
    let together = left(&render(ID, 128, &chord));
    let mut sum = vec![0.0_f32; FRAMES];
    for key in [60, 64, 67] {
        for (s, x) in sum.iter_mut().zip(left(&render(ID, 128, &notes(key)))) {
            *s += x;
        }
    }
    for (i, (a, b)) in together.iter().zip(&sum).enumerate() {
        assert!((a - b).abs() < 1e-6, "frame {i}: {a} vs {b}");
    }
}

/// Renders the same events through both plugins and checks they agree
/// sample for sample: the host's pedal for the notes-only plugin has to do
/// exactly what the other plugin does with the pedal itself.
fn both_agree(events: &[TimedEvent]) -> Vec<f32> {
    let midi = render(ID, 128, events);
    let notes_only = render(NOTES_ONLY_ID, 128, events);
    for (i, (a, b)) in midi.iter().zip(&notes_only).enumerate() {
        assert_eq!(a, b, "sample {i}");
    }
    left(&midi)
}

#[test]
fn the_sustain_pedal_holds_a_released_note_until_it_comes_up() {
    let l = both_agree(&[
        on(0.1, 69, 1.0),
        cc(0.2, SUSTAIN, 127),
        off(0.3, 69),
        cc(0.5, SUSTAIN, 0),
    ]);
    // Still at full level after the key came up at 0.3 s...
    assert!((peak(between(&l, 0.3, 0.5)) - PEAK).abs() < 1e-3);
    // ...and silent once the pedal came up at 0.5 s and the fade ended.
    assert!(silent(between(&l, 0.5 + FADE + 0.001, 0.8)));
}

#[test]
fn a_key_released_before_the_pedal_goes_down_is_not_held() {
    let l = both_agree(&[on(0.1, 69, 1.0), off(0.2, 69), cc(0.3, SUSTAIN, 127)]);
    assert!(silent(between(&l, 0.2 + FADE + 0.001, 0.8)));
}

#[test]
fn striking_a_held_key_again_does_not_pile_up_voices() {
    let l = both_agree(&[
        cc(0.05, SUSTAIN, 127),
        on(0.1, 69, 1.0),
        off(0.2, 69),
        on(0.3, 69, 1.0),
        off(0.4, 69),
        cc(0.6, SUSTAIN, 0),
    ]);
    // Two voices of the same key would reach twice the level.
    assert!(peak(&l) <= PEAK + 1e-3, "peak {}", peak(&l));
    assert!(silent(between(&l, 0.6 + FADE + 0.001, 0.8)));
}

#[test]
fn all_notes_off_releases_held_notes_too() {
    let l = both_agree(&[
        cc(0.05, SUSTAIN, 127),
        on(0.1, 60, 1.0),
        on(0.1, 64, 1.0),
        off(0.2, 60),
        cc(0.4, ALL_NOTES_OFF, 0),
    ]);
    assert!(peak(between(&l, 0.3, 0.4)) > PEAK);
    assert!(silent(between(&l, 0.4 + FADE + 0.001, 0.8)));
}

#[test]
fn pitch_bend_reaches_a_plugin_that_takes_midi() {
    let events = [on(0.1, 69, 1.0), bend(0.3, 16_383), off(0.7, 69)];
    let l = left(&render(ID, 128, &events));
    let window = 1.0 / 0.2;
    let before = frequency(between(&l, 0.1 + FADE, 0.3));
    assert!((before - 440.0).abs() <= window, "{before} Hz");
    // Full bend up: 8191 of 8192 steps towards two semitones.
    let semitones = 8191.0 / 8192.0 * BEND_SEMITONES;
    let expected = key_frequency(69) * 2f64.powf(semitones / 12.0);
    let after = frequency(between(&l, 0.3, 0.7));
    assert!(
        (after - expected).abs() <= window / 2.0,
        "{after} Hz, expected {expected}"
    );
}

#[test]
fn a_notes_only_plugin_ignores_pitch_bend() {
    let events = [
        on(0.1, 69, 1.0),
        bend(0.3, 16_383),
        bend(0.5, PITCH_BEND_CENTRE),
        off(0.7, 69),
    ];
    let bent = render(NOTES_ONLY_ID, 128, &events);
    let plain = render(NOTES_ONLY_ID, 128, &[on(0.1, 69, 1.0), off(0.7, 69)]);
    assert_eq!(bent, plain);
}
