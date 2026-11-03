//! A small CLAP instrument for testing refrain's plugin hosting.
//!
//! It plays [`refrain_test_tone::Tone`], the same sound as the VST3 test
//! plugin, so a host can check what it hears against what it sent, and both
//! plugin formats against each other.
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

use refrain_test_tone::Tone;
pub use refrain_test_tone::{BEND_SEMITONES, FADE_SECONDS, PEAK, RIGHT_GAIN, key_frequency};

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

pub struct SynthProcessor<'a> {
    shared: &'a SynthShared,
    tone: Tone,
}

impl SynthProcessor<'_> {
    fn handle(&mut self, event: &UnknownEvent) {
        let specific = |m: Match<u16>| match m {
            Match::Specific(v) => Some(v),
            Match::All => None,
        };
        match event.as_core_event() {
            Some(CoreEventSpace::NoteOn(e)) => {
                if let (Match::Specific(channel), Match::Specific(key)) = (e.channel(), e.key()) {
                    self.tone.note_on(channel, key, e.velocity() as f32);
                }
            }
            Some(CoreEventSpace::NoteOff(e)) => {
                self.tone.note_off(specific(e.channel()), specific(e.key()));
            }
            Some(CoreEventSpace::NoteChoke(e)) => {
                self.tone.choke(specific(e.channel()), specific(e.key()));
            }
            Some(CoreEventSpace::Midi(e)) if self.shared.takes_midi => self.tone.midi(e.data()),
            _ => {}
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
            tone: Tone::new(config.sample_rate),
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
            self.tone.render(&mut left[batch.sample_bounds()]);
        }

        if channels.channel_count() > 1 {
            let (first, rest) = channels.split_at_mut(1);
            let first = first.channel(0).unwrap();
            for other in rest {
                Tone::write_right(first, other);
            }
        }

        if self.tone.is_sounding() {
            Ok(ProcessStatus::Continue)
        } else {
            Ok(ProcessStatus::Sleep)
        }
    }

    fn stop_processing(&mut self) {
        self.tone.stop();
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
