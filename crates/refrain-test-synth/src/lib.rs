//! A small CLAP instrument for testing refrain's plugin hosting.
//!
//! Each note is a sine wave at the note's pitch, with an amplitude of
//! `velocity * PEAK` and a linear 5 ms fade in and out. The output is stereo:
//! the right channel is the left one times [`RIGHT_GAIN`], so a host that
//! swaps or duplicates channels shows up in tests. Everything is
//! deterministic, so a host can check what it hears against what it sent.
//!
//! The file holds two plugins that sound the same:
//!
//! - [`ID`] takes CLAP notes and MIDI. It plays the sustain pedal, pitch bend
//!   (two semitones each way) and All Notes Off itself.
//! - [`NOTES_ONLY_ID`] takes CLAP notes and nothing else, so the host has to
//!   play the sustain pedal for it.

use std::ffi::CStr;

use clack_extensions::audio_ports::{
    AudioPortFlags, AudioPortInfo, AudioPortInfoWriter, AudioPortType, PluginAudioPorts,
    PluginAudioPortsImpl,
};
use clack_extensions::note_ports::{
    NoteDialect, NoteDialects, NotePortInfo, NotePortInfoWriter, PluginNotePorts,
    PluginNotePortsImpl,
};
use clack_plugin::entry::prelude::*;
use clack_plugin::events::Match;
use clack_plugin::events::spaces::CoreEventSpace;
use clack_plugin::prelude::*;

/// The plugin that takes MIDI as well as notes.
pub const ID: &str = "dev.refrain.test-synth";

/// The plugin that takes notes only.
pub const NOTES_ONLY_ID: &str = "dev.refrain.test-synth.notes-only";

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

pub struct TestSynth;

impl Plugin for TestSynth {
    type AudioProcessor<'a> = SynthProcessor<'a>;
    type Shared<'a> = SynthShared;
    type MainThread<'a> = SynthMainThread<'a>;

    fn declare_extensions(builder: &mut PluginExtensions<Self>, _shared: Option<&SynthShared>) {
        builder
            .register::<PluginAudioPorts>()
            .register::<PluginNotePorts>();
    }
}

/// What differs between the two plugins.
pub struct SynthShared {
    takes_midi: bool,
}

impl PluginShared<'_> for SynthShared {}

pub struct SynthMainThread<'a> {
    shared: &'a SynthShared,
}

impl<'a> PluginMainThread<'a, SynthShared> for SynthMainThread<'a> {}

/// The CLAP entry of this file, for loading it in-process.
pub struct Entry {
    factory: PluginFactoryWrapper<Factory>,
}

impl clack_plugin::entry::Entry for Entry {
    fn new(_bundle_path: Option<&CStr>) -> Result<Self, EntryLoadError> {
        use clack_plugin::plugin::features::{INSTRUMENT, STEREO, SYNTHESIZER};
        let describe = |id, name| {
            PluginDescriptor::new(id, name).with_features([INSTRUMENT, SYNTHESIZER, STEREO])
        };
        Ok(Self {
            factory: PluginFactoryWrapper::new(Factory {
                full: describe(ID, "refrain test synth"),
                notes_only: describe(NOTES_ONLY_ID, "refrain test synth (notes only)"),
            }),
        })
    }

    fn declare_factories<'a>(&'a self, builder: &mut EntryFactories<'a>) {
        builder.register_factory(&self.factory);
    }
}

struct Factory {
    full: PluginDescriptor,
    notes_only: PluginDescriptor,
}

impl PluginFactoryImpl for Factory {
    fn plugin_count(&self) -> u32 {
        2
    }

    fn plugin_descriptor(&self, index: u32) -> Option<&PluginDescriptor> {
        match index {
            0 => Some(&self.full),
            1 => Some(&self.notes_only),
            _ => None,
        }
    }

    fn create_plugin<'a>(
        &'a self,
        host_info: HostInfo<'a>,
        plugin_id: &CStr,
    ) -> Option<PluginInstance<'a>> {
        let (descriptor, takes_midi) = if Some(plugin_id) == self.full.id() {
            (&self.full, true)
        } else if Some(plugin_id) == self.notes_only.id() {
            (&self.notes_only, false)
        } else {
            return None;
        };
        Some(PluginInstance::new::<TestSynth>(
            host_info,
            descriptor,
            move |_host| Ok(SynthShared { takes_midi }),
            |_host, shared| Ok(SynthMainThread { shared }),
        ))
    }
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

pub struct SynthProcessor<'a> {
    shared: &'a SynthShared,
    voices: [Voice; VOICES],
    sample_rate: f64,
    fade_step: f32,
    pedal_down: [bool; 16],
    // Pitch factor per MIDI channel, from pitch bend.
    bend: [f64; 16],
}

impl SynthProcessor<'_> {
    fn note_on(&mut self, channel: u16, key: u16, velocity: f64) {
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
            gain: velocity as f32 * PEAK,
            level: 0.0,
            releasing: false,
            held: false,
        };
    }

    fn note_off(&mut self, channel: Match<u16>, key: Match<u16>) {
        let pedal = self.pedal_down;
        for v in self.voices.iter_mut().filter(|v| v.active && !v.releasing) {
            if channel.matches(v.channel) && key.matches(v.key) {
                if pedal[usize::from(v.channel & 0x0f)] {
                    v.held = true;
                } else {
                    v.releasing = true;
                }
            }
        }
    }

    fn release(&mut self, pick: impl Fn(&Voice) -> bool) {
        for v in self.voices.iter_mut().filter(|v| v.active && pick(v)) {
            v.releasing = true;
            v.held = false;
        }
    }

    fn midi(&mut self, [status, a, b]: [u8; 3]) {
        let channel = u16::from(status & 0x0f);
        let c = usize::from(channel);
        match (status & 0xf0, a) {
            (0x90, _) if b > 0 => self.note_on(channel, a.into(), f64::from(b) / 127.0),
            (0x80 | 0x90, _) => self.note_off(Match::Specific(channel), Match::Specific(a.into())),
            (0xb0, 64) => {
                let down = b >= 64;
                if self.pedal_down[c] && !down {
                    self.release(|v| v.channel == channel && v.held);
                }
                self.pedal_down[c] = down;
            }
            (0xb0, 123) => self.release(|v| v.channel == channel),
            (0xe0, _) => {
                let value = (u16::from(b) << 7 | u16::from(a)) as f64;
                let semitones = (value - 8192.0) / 8192.0 * BEND_SEMITONES;
                self.bend[c] = 2f64.powf(semitones / 12.0);
            }
            _ => {}
        }
    }

    fn handle(&mut self, event: &UnknownEvent) {
        match event.as_core_event() {
            Some(CoreEventSpace::NoteOn(e)) => {
                if let (Match::Specific(channel), Match::Specific(key)) = (e.channel(), e.key()) {
                    self.note_on(channel, key, e.velocity());
                }
            }
            Some(CoreEventSpace::NoteOff(e)) => self.note_off(e.channel(), e.key()),
            Some(CoreEventSpace::NoteChoke(e)) => {
                for v in self.voices.iter_mut() {
                    if e.channel().matches(v.channel) && e.key().matches(v.key) {
                        v.active = false;
                    }
                }
            }
            Some(CoreEventSpace::Midi(e)) if self.shared.takes_midi => self.midi(e.data()),
            _ => {}
        }
    }

    fn render(&mut self, out: &mut [f32]) {
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
}

impl<'a> PluginAudioProcessor<'a, SynthShared, SynthMainThread<'a>> for SynthProcessor<'a> {
    fn activate(
        _host: HostAudioProcessorHandle<'a>,
        _main_thread: &SynthMainThread,
        shared: &'a SynthShared,
        config: PluginAudioConfiguration,
    ) -> Result<Self, PluginError> {
        Ok(Self {
            shared,
            voices: [Voice::default(); VOICES],
            sample_rate: config.sample_rate,
            fade_step: 1.0 / (FADE_SECONDS * config.sample_rate as f32),
            pedal_down: [false; 16],
            bend: [1.0; 16],
        })
    }

    fn process(
        &mut self,
        _process: Process,
        mut audio: Audio,
        events: Events,
    ) -> Result<ProcessStatus, PluginError> {
        let mut port = audio
            .output_port(0)
            .ok_or(PluginError::Message("no output port"))?;
        let mut channels = port
            .channels()?
            .into_f32()
            .ok_or(PluginError::Message("expected f32 output"))?;
        let left = channels
            .channel_mut(0)
            .ok_or(PluginError::Message("expected an output channel"))?;
        left.fill(0.0);

        for batch in events.input.batch() {
            for event in batch.events() {
                self.handle(event);
            }
            self.render(&mut left[batch.sample_bounds()]);
        }

        if channels.channel_count() > 1 {
            let (first, rest) = channels.split_at_mut(1);
            let first = first.channel(0).unwrap();
            for other in rest {
                for (o, &s) in other.iter_mut().zip(first.iter()) {
                    *o = s * RIGHT_GAIN;
                }
            }
        }

        if self.voices.iter().any(|v| v.active) {
            Ok(ProcessStatus::Continue)
        } else {
            Ok(ProcessStatus::Sleep)
        }
    }

    fn stop_processing(&mut self) {
        for v in self.voices.iter_mut() {
            v.active = false;
        }
    }
}

impl PluginAudioPortsImpl for SynthMainThread<'_> {
    fn count(&self, is_input: bool) -> u32 {
        if is_input { 0 } else { 1 }
    }

    fn get(&self, index: u32, is_input: bool, writer: &mut AudioPortInfoWriter) {
        if !is_input && index == 0 {
            writer.set(&AudioPortInfo {
                id: ClapId::new(0),
                name: b"main",
                channel_count: 2,
                flags: AudioPortFlags::IS_MAIN,
                port_type: Some(AudioPortType::STEREO),
                in_place_pair: None,
            });
        }
    }
}

impl PluginNotePortsImpl for SynthMainThread<'_> {
    fn count(&self, is_input: bool) -> u32 {
        if is_input { 1 } else { 0 }
    }

    fn get(&self, index: u32, is_input: bool, writer: &mut NotePortInfoWriter) {
        if is_input && index == 0 {
            let supported_dialects = if self.shared.takes_midi {
                NoteDialects::CLAP | NoteDialects::MIDI
            } else {
                NoteDialects::CLAP
            };
            writer.set(&NotePortInfo {
                id: ClapId::new(0),
                name: b"notes",
                preferred_dialect: Some(NoteDialect::Clap),
                supported_dialects,
            });
        }
    }
}

clack_export_entry!(Entry);
