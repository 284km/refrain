//! Hosts refrain-test-synth-vst3 in-process and checks what comes out.
//!
//! The VST3 test plugins play the same sound as the CLAP ones, so the
//! strongest check is that the same performance gives the same samples
//! through both: the CLAP host and the VST3 host are each other's reference.
//! The VST3 plugin that maps controllers plays the pedal itself; for the one
//! that takes notes only, the host plays it. All three must agree.

use clack_host::prelude::PluginEntry;
use refrain_clap::ClapPlugin;
use refrain_engine::{
    ALL_NOTES_OFF, Event, PITCH_BEND_CENTRE, SUSTAIN, TimedEvent, render_offline,
};
use refrain_test_synth_vst3::{FADE_SECONDS, NAME, NOTES_ONLY_NAME, PEAK, RIGHT_GAIN};
use refrain_vst3::{Vst3Error, Vst3Module, Vst3Plugin};

const RATE: f32 = 48_000.0;
const FRAMES: usize = 38_400; // 0.8 s

fn module() -> Vst3Module {
    unsafe { Vst3Module::from_factory(refrain_test_synth_vst3::GetPluginFactory()) }.unwrap()
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

fn render_vst3(name: &str, channels: usize, block: usize, events: &[TimedEvent]) -> Vec<f32> {
    let module = module();
    let mut plugin = Vst3Plugin::new(&module, Some(name)).unwrap();
    let mut processor = plugin.activate(RATE, 1_024).unwrap();
    let out = render_offline(&mut processor, RATE, channels, FRAMES, block, events);
    assert!(!processor.has_failed());
    drop(processor);
    drop(plugin);
    out
}

/// The same performance through the CLAP test plugin that takes MIDI.
fn render_clap(block: usize, events: &[TimedEvent]) -> Vec<f32> {
    let entry =
        PluginEntry::load_from_clack::<refrain_test_synth::Entry>(c"refrain-test-synth").unwrap();
    let mut plugin = ClapPlugin::new(&entry, Some(refrain_test_synth::ID)).unwrap();
    let mut processor = plugin.activate(RATE, 1_024).unwrap();
    let out = render_offline(&mut processor, RATE, 2, FRAMES, block, events);
    drop(processor);
    drop(plugin);
    out
}

fn left(out: &[f32]) -> Vec<f32> {
    out.iter().step_by(2).copied().collect()
}

fn between(l: &[f32], from: f64, to: f64) -> &[f32] {
    &l[s(from) as usize..s(to) as usize]
}

fn peak(x: &[f32]) -> f32 {
    x.iter().fold(0.0_f32, |m, s| m.max(s.abs()))
}

fn silent(x: &[f32]) -> bool {
    x.iter().all(|&s| s == 0.0)
}

const FADE: f64 = FADE_SECONDS as f64;

/// Renders through the CLAP plugin and both VST3 plugins, and requires the
/// same samples from all three.
fn all_agree(events: &[TimedEvent]) -> Vec<f32> {
    let clap = render_clap(128, events);
    for name in [NAME, NOTES_ONLY_NAME] {
        let vst3 = render_vst3(name, 2, 128, events);
        assert_eq!(vst3.len(), clap.len());
        if let Some(i) = vst3.iter().zip(&clap).position(|(a, b)| a != b) {
            panic!("{name}: sample {i} is {}, CLAP gives {}", vst3[i], clap[i]);
        }
    }
    left(&clap)
}

#[test]
fn lists_both_instruments() {
    let plugins = module().plugins();
    let names: Vec<&str> = plugins.iter().map(|p| p.name.as_str()).collect();
    assert_eq!(names, [NAME, NOTES_ONLY_NAME]);
    for p in &plugins {
        assert_eq!(p.categories, "Instrument|Synth");
        assert_eq!(p.vendor, "refrain");
        assert_eq!(p.id.len(), 32);
    }
    assert_ne!(plugins[0].id, plugins[1].id);
}

#[test]
fn choosing_a_plugin_needs_a_name_when_the_module_holds_several() {
    let err = Vst3Plugin::new(&module(), None).err().unwrap();
    assert!(matches!(err, Vst3Error::Ambiguous(_)), "{err}");
    let err = Vst3Plugin::new(&module(), Some("no such plugin"))
        .err()
        .unwrap();
    assert!(matches!(err, Vst3Error::NotFound(_)), "{err}");
    // The class ID works as well as the name.
    let id = module().plugins()[1].id.clone();
    let plugin = Vst3Plugin::new(&module(), Some(&id)).unwrap();
    assert_eq!(plugin.info().name, NOTES_ONLY_NAME);
}

#[test]
fn a_note_sounds_the_same_as_through_clap() {
    let l = all_agree(&[on(0.1, 69, 1.0), off(0.6, 69)]);
    assert!(silent(between(&l, 0.0, 0.1)));
    assert!((peak(between(&l, 0.1 + FADE, 0.6)) - PEAK).abs() < 1e-3);
    assert!(silent(between(&l, 0.6 + FADE + 0.001, 0.8)));
}

#[test]
fn the_right_channel_comes_through() {
    let out = render_vst3(NAME, 2, 128, &[on(0.1, 69, 1.0), off(0.6, 69)]);
    assert!(peak(&out) > 0.0);
    for frame in out.as_chunks::<2>().0 {
        assert_eq!(frame[1], frame[0] * RIGHT_GAIN);
    }
}

#[test]
fn the_sustain_pedal_sounds_the_same_as_through_clap() {
    let l = all_agree(&[
        on(0.1, 69, 1.0),
        cc(0.2, SUSTAIN, 127),
        off(0.3, 69),
        cc(0.5, SUSTAIN, 0),
    ]);
    assert!((peak(between(&l, 0.3, 0.5)) - PEAK).abs() < 1e-3);
    assert!(silent(between(&l, 0.5 + FADE + 0.001, 0.8)));
}

#[test]
fn a_held_key_struck_again_sounds_the_same_as_through_clap() {
    let l = all_agree(&[
        cc(0.05, SUSTAIN, 127),
        on(0.1, 69, 1.0),
        off(0.2, 69),
        on(0.3, 69, 1.0),
        off(0.4, 69),
        cc(0.6, SUSTAIN, 0),
    ]);
    assert!(peak(&l) <= PEAK + 1e-3);
}

#[test]
fn all_notes_off_sounds_the_same_as_through_clap() {
    let l = all_agree(&[
        cc(0.05, SUSTAIN, 127),
        on(0.1, 60, 1.0),
        on(0.1, 64, 1.0),
        off(0.2, 60),
        cc(0.4, ALL_NOTES_OFF, 0),
    ]);
    assert!(silent(between(&l, 0.4 + FADE + 0.001, 0.8)));
}

#[test]
fn pitch_bend_reaches_the_plugin_that_maps_it() {
    let events = [
        on(0.1, 69, 1.0),
        bend(0.3, 16_383),
        bend(0.5, 2_000),
        off(0.7, 69),
    ];
    let clap = render_clap(128, &events);
    let vst3 = render_vst3(NAME, 2, 128, &events);
    assert_eq!(vst3, clap);
    let unbent = render_vst3(NAME, 2, 128, &[on(0.1, 69, 1.0), off(0.7, 69)]);
    assert_ne!(vst3, unbent);
}

#[test]
fn the_notes_only_plugin_ignores_pitch_bend() {
    let bent = render_vst3(
        NOTES_ONLY_NAME,
        2,
        128,
        &[
            on(0.1, 69, 1.0),
            bend(0.3, 16_383),
            bend(0.5, PITCH_BEND_CENTRE),
            off(0.7, 69),
        ],
    );
    let plain = render_vst3(NOTES_ONLY_NAME, 2, 128, &[on(0.1, 69, 1.0), off(0.7, 69)]);
    assert_eq!(bent, plain);
}

#[test]
fn output_does_not_depend_on_block_size() {
    let events = [
        cc(0.05, SUSTAIN, 127),
        on(0.1, 69, 1.0),
        off(0.3, 69),
        bend(0.35, 12_000),
        cc(0.5, SUSTAIN, 0),
    ];
    for name in [NAME, NOTES_ONLY_NAME] {
        let reference = render_vst3(name, 2, 1_024, &events);
        for block in [1, 64, 100, 128, 1_000] {
            assert_eq!(
                render_vst3(name, 2, block, &events),
                reference,
                "{name}, block {block}"
            );
        }
    }
}

#[test]
fn stereo_is_mixed_down_for_mono_and_padded_for_more_channels() {
    let events = [on(0.1, 69, 1.0), off(0.6, 69)];
    let stereo = render_vst3(NAME, 2, 128, &events);
    let mono = render_vst3(NAME, 1, 128, &events);
    for (m, lr) in mono.iter().zip(stereo.as_chunks::<2>().0) {
        assert_eq!(*m, (lr[0] + lr[1]) / 2.0);
    }
    let quad = render_vst3(NAME, 4, 128, &events);
    for (q, lr) in quad
        .as_chunks::<4>()
        .0
        .iter()
        .zip(stereo.as_chunks::<2>().0)
    {
        assert_eq!(q, &[lr[0], lr[1], 0.0, 0.0]);
    }
}
