//! Hosts refrain-test-synth-vst3 in-process and checks what comes out.
//!
//! The VST3 test plugins play the same sound as the CLAP ones, so the
//! strongest check is that the same performance gives the same samples
//! through both: the CLAP host and the VST3 host are each other's reference.
//! The VST3 plugin that maps controllers plays the pedal itself; for the one
//! that takes notes only, the host plays it. All three must agree.
//!
//! The module also holds an effect that doubles and clips, like the CLAP test
//! effect. Chains of them must sound the same in either format, and in a mix
//! of both.

use clack_host::prelude::PluginEntry;
use refrain_clap::ClapPlugin;
use refrain_engine::{
    ALL_NOTES_OFF, Event, Mix, Mixer, PITCH_BEND_CENTRE, Processor, SUSTAIN, TimedEvent, Track,
    render_offline,
};
use refrain_test_synth_vst3::{EFFECT_NAME, FADE_SECONDS, NAME, NOTES_ONLY_NAME, PEAK, RIGHT_GAIN};
use refrain_test_tone::effect;
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
fn lists_both_instruments_and_the_effect() {
    let plugins = module().plugins();
    let names: Vec<&str> = plugins.iter().map(|p| p.name.as_str()).collect();
    assert_eq!(names, [NAME, NOTES_ONLY_NAME, EFFECT_NAME]);
    let categories: Vec<&str> = plugins.iter().map(|p| p.categories.as_str()).collect();
    assert_eq!(categories, ["Instrument|Synth", "Instrument|Synth", "Fx"]);
    for p in &plugins {
        assert_eq!(p.vendor, "refrain");
        assert_eq!(p.id.len(), 32);
    }
    assert_ne!(plugins[0].id, plugins[1].id);
    assert_ne!(plugins[1].id, plugins[2].id);
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

/// Runs `processor` block by block, calling `before` with each block's index
/// first. Events carry frames counted from the start, as for render_offline.
fn render_with(
    processor: &mut dyn refrain_engine::Processor,
    block: usize,
    events: &[TimedEvent],
    mut before: impl FnMut(usize),
) -> Vec<f32> {
    processor.prepare(RATE, block);
    let mut out = vec![0.0; FRAMES * 2];
    for (b, chunk) in out.chunks_mut(block * 2).enumerate() {
        before(b);
        let (start, end) = ((b * block) as u32, (b * block + chunk.len() / 2) as u32);
        let here: Vec<TimedEvent> = events
            .iter()
            .filter(|e| (start..end).contains(&e.frame))
            .map(|e| TimedEvent {
                frame: e.frame - start,
                ..*e
            })
            .collect();
        processor.process(chunk, 2, &here);
    }
    out
}

#[test]
fn edits_in_the_editor_reach_the_processor() {
    use refrain_test_synth_vst3::{edit_from_editor, params};
    let block = 128;
    let notes = [on(0.1, 69, 1.0), off(0.3, 69)];
    let (down, up) = (s(0.2) as usize / block, s(0.5) as usize / block);

    // The pedal from the editor, which the host applies at a block's start...
    let module = module();
    let mut plugin = Vst3Plugin::new(&module, Some(NAME)).unwrap();
    let mut processor = plugin.activate(RATE, 1_024).unwrap();
    let edited = render_with(&mut processor, block, &notes, |b| {
        if b == down {
            edit_from_editor(params::SUSTAIN, 1.0);
        } else if b == up {
            edit_from_editor(params::SUSTAIN, 0.0);
        }
    });
    drop(processor);
    drop(plugin);

    // ...sounds like the pedal from MIDI at those same samples.
    let mut with_cc = notes.to_vec();
    with_cc.push(TimedEvent {
        frame: (down * block) as u32,
        event: cc(0.0, SUSTAIN, 127).event,
    });
    with_cc.push(TimedEvent {
        frame: (up * block) as u32,
        event: cc(0.0, SUSTAIN, 0).event,
    });
    with_cc.sort_by_key(|e| e.frame);
    assert_eq!(edited, render_vst3(NAME, 2, block, &with_cc));
    assert_ne!(
        edited,
        render_vst3(NAME, 2, block, &notes),
        "the edit had no effect"
    );
}

/// Renders through a fresh VST3 plugin, after loading `state` if given, and
/// returns the output and the plugin's state afterwards.
fn vst3_with_state(
    name: &str,
    state: Option<&refrain_vst3::Vst3State>,
    events: &[TimedEvent],
) -> (Vec<f32>, refrain_vst3::Vst3State) {
    let module = module();
    let mut plugin = Vst3Plugin::new(&module, Some(name)).unwrap();
    if let Some(state) = state {
        plugin.load_state(state).unwrap();
    }
    let mut processor = plugin.activate(RATE, 1_024).unwrap();
    let out = render_offline(&mut processor, RATE, 2, FRAMES, 128, events);
    drop(processor);
    let saved = plugin.save_state().unwrap();
    (out, saved)
}

#[test]
fn a_saved_state_brings_the_sound_back() {
    use refrain_test_synth_vst3::VOLUME_CONTROLLER;
    let a4 = [on(0.1, 69, 1.0), off(0.6, 69)];
    let mut turned_down = vec![cc(0.0, VOLUME_CONTROLLER, 64)];
    turned_down.extend(a4);
    let (quieter, saved) = vst3_with_state(NAME, None, &turned_down);
    assert_eq!(saved.component, refrain_test_tone::save_state(64.0 / 127.0));

    let (restored, resaved) = vst3_with_state(NAME, Some(&saved), &a4);
    assert_eq!(restored, quieter);
    assert_eq!(resaved, saved);
    let (default, _) = vst3_with_state(NAME, None, &a4);
    assert_ne!(restored, default);

    // The CLAP plugin turned down the same way sounds the same.
    assert_eq!(quieter, render_clap(128, &turned_down));
}

#[test]
fn a_state_saved_by_the_clap_plugin_restores_in_the_vst3_one() {
    // The test plugins share one state format, so this crosses formats. Real
    // plugins do not promise that; this checks both hosts pass bytes intact.
    let entry =
        PluginEntry::load_from_clack::<refrain_test_synth::Entry>(c"refrain-test-synth").unwrap();
    let mut clap = ClapPlugin::new(&entry, Some(refrain_test_synth::NOTES_ONLY_ID)).unwrap();
    clap.load_state(&refrain_test_tone::save_state(0.25))
        .unwrap();
    let bytes = clap.save_state().unwrap().unwrap();
    let state = refrain_vst3::Vst3State {
        component: bytes,
        controller: Vec::new(),
    };
    let a4 = [on(0.1, 69, 1.0), off(0.6, 69)];
    let (restored, _) = vst3_with_state(NOTES_ONLY_NAME, Some(&state), &a4);
    let (default, _) = vst3_with_state(NOTES_ONLY_NAME, None, &a4);
    for (r, d) in restored.iter().zip(&default) {
        assert_eq!(*r, *d * 0.25);
    }
}

#[test]
fn a_broken_state_is_refused() {
    let module = module();
    let mut plugin = Vst3Plugin::new(&module, Some(NAME)).unwrap();
    let broken = refrain_vst3::Vst3State {
        component: b"nope".to_vec(),
        controller: Vec::new(),
    };
    let err = plugin.load_state(&broken).unwrap_err();
    assert!(matches!(err, Vst3Error::State(_)), "{err}");
}

/// A plugin in a chain: the synth or the effect, in either format, with the
/// effect's gain (`None` for the default).
#[derive(Clone, Copy)]
enum Slot {
    ClapSynth,
    Vst3Synth,
    ClapEffect(Option<f32>),
    Vst3Effect(Option<f32>),
}

/// Plays `events` through a track of `slots`, to a device with `channels`
/// channels.
fn render_chain(slots: &[Slot], channels: usize, events: &[TimedEvent]) -> Vec<f32> {
    let entry =
        PluginEntry::load_from_clack::<refrain_test_synth::Entry>(c"refrain-test-synth").unwrap();
    let module = module();
    let mut claps = Vec::new();
    let mut vst3s = Vec::new();
    let mut chain: Vec<Box<dyn Processor>> = Vec::new();
    for slot in slots {
        match *slot {
            Slot::ClapSynth | Slot::ClapEffect(_) => {
                let id = match slot {
                    Slot::ClapSynth => refrain_test_synth::ID,
                    _ => refrain_test_synth::EFFECT_ID,
                };
                let mut plugin = ClapPlugin::new(&entry, Some(id)).unwrap();
                if let Slot::ClapEffect(Some(gain)) = slot {
                    plugin.load_state(&effect::save_state(*gain)).unwrap();
                }
                chain.push(Box::new(plugin.activate(RATE, 1_024).unwrap()));
                claps.push(plugin);
            }
            Slot::Vst3Synth | Slot::Vst3Effect(_) => {
                let name = match slot {
                    Slot::Vst3Synth => NAME,
                    _ => EFFECT_NAME,
                };
                let mut plugin = Vst3Plugin::new(&module, Some(name)).unwrap();
                if let Slot::Vst3Effect(Some(gain)) = slot {
                    let state = refrain_vst3::Vst3State {
                        component: effect::save_state(*gain).to_vec(),
                        controller: Vec::new(),
                    };
                    plugin.load_state(&state).unwrap();
                }
                chain.push(Box::new(plugin.activate(RATE, 1_024).unwrap()));
                vst3s.push(plugin);
            }
        }
    }
    let mut mixer =
        Mixer::new(vec![Track::new(chain, Mix::default())], Vec::new(), 0.0).with_live_track(0);
    let out = render_offline(&mut mixer, RATE, channels, FRAMES, 128, events);
    assert!(!mixer.has_failed());
    // The processors go before their plugins.
    drop(mixer);
    out
}

/// A chord loud enough for doubling to clip.
fn chord() -> Vec<TimedEvent> {
    let mut events = vec![on(0.1, 60, 1.0), on(0.1, 64, 1.0), on(0.1, 67, 1.0)];
    events.extend([off(0.6, 60), off(0.6, 64), off(0.6, 67)]);
    events
}

#[test]
fn an_effect_changes_what_comes_before_it() {
    let mut expected = render_vst3(NAME, 2, 128, &chord());
    effect::process(effect::DEFAULT_GAIN, &mut expected);
    let out = render_chain(&[Slot::Vst3Synth, Slot::Vst3Effect(None)], 2, &chord());
    assert_eq!(out, expected);
}

#[test]
fn chains_sound_the_same_in_either_format_and_mixed() {
    use Slot::*;
    let (half, double) = (Some(0.5), Some(2.0));
    let clap = render_chain(
        &[ClapSynth, ClapEffect(double), ClapEffect(half)],
        2,
        &chord(),
    );
    let vst3 = render_chain(
        &[Vst3Synth, Vst3Effect(double), Vst3Effect(half)],
        2,
        &chord(),
    );
    let mixed = render_chain(
        &[ClapSynth, Vst3Effect(double), ClapEffect(half)],
        2,
        &chord(),
    );
    assert_eq!(vst3, clap);
    assert_eq!(mixed, clap);
    // The other order sounds different.
    let swapped = render_chain(
        &[Vst3Synth, Vst3Effect(half), Vst3Effect(double)],
        2,
        &chord(),
    );
    assert_ne!(swapped, vst3);
}

#[test]
fn a_mono_device_feeds_an_effect_on_both_channels() {
    let mut expected = render_vst3(NAME, 1, 128, &chord());
    effect::process(effect::DEFAULT_GAIN, &mut expected);
    let out = render_chain(&[Slot::Vst3Synth, Slot::Vst3Effect(None)], 1, &chord());
    assert_eq!(out, expected);
}

#[test]
fn an_effect_keeps_its_gain_in_its_state() {
    let module = module();
    let mut plugin = Vst3Plugin::new(&module, Some(EFFECT_NAME)).unwrap();
    let saved = plugin.save_state().unwrap();
    assert_eq!(saved.component, effect::save_state(effect::DEFAULT_GAIN));
    let half = refrain_vst3::Vst3State {
        component: effect::save_state(0.5).to_vec(),
        controller: Vec::new(),
    };
    plugin.load_state(&half).unwrap();
    assert_eq!(plugin.save_state().unwrap().component, half.component);
    let synth_state = refrain_vst3::Vst3State {
        component: refrain_test_tone::save_state(0.5).to_vec(),
        controller: Vec::new(),
    };
    let err = plugin.load_state(&synth_state).unwrap_err();
    assert!(matches!(err, Vst3Error::State(_)), "{err}");
}

#[test]
fn offline_rendering_is_set_up_and_sounds_the_same() {
    use refrain_test_synth_vst3::set_up_offline;
    let module = module();
    for name in [NAME, EFFECT_NAME] {
        let mut plugin = Vst3Plugin::new(&module, Some(name)).unwrap();
        plugin.set_offline(true);
        let mut processor = plugin.activate(RATE, 1_024).unwrap();
        assert_eq!(set_up_offline(), Some(true), "{name}");
        // The plugins fail a block whose mode differs from the set-up one.
        let out = render_offline(&mut processor, RATE, 2, FRAMES, 128, &chord());
        assert!(!processor.has_failed(), "{name}");
        drop(processor);
        if name == NAME {
            assert_eq!(out, render_vst3(NAME, 2, 128, &chord()));
            assert_eq!(set_up_offline(), Some(false));
        }
    }
}
