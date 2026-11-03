//! A small CLAP instrument for testing refrain's plugin hosting.
//!
//! Each note is a sine wave at the note's pitch, with an amplitude of
//! `velocity * PEAK` and a linear 5 ms fade in and out. The output is stereo:
//! the right channel is the left one times [`RIGHT_GAIN`], so a host that
//! swaps or duplicates channels shows up in tests. Everything is
//! deterministic, so a host can check what it hears against what it sent.

use clack_extensions::audio_ports::{
    AudioPortFlags, AudioPortInfo, AudioPortInfoWriter, AudioPortType, PluginAudioPorts,
    PluginAudioPortsImpl,
};
use clack_extensions::note_ports::{
    NoteDialect, NoteDialects, NotePortInfo, NotePortInfoWriter, PluginNotePorts,
    PluginNotePortsImpl,
};
use clack_plugin::events::Match;
use clack_plugin::events::spaces::CoreEventSpace;
use clack_plugin::prelude::*;

/// The plugin ID, as hosts see it.
pub const ID: &str = "dev.refrain.test-synth";

/// Amplitude of a note at full velocity.
pub const PEAK: f32 = 0.2;

/// The right channel is the left channel times this. A power of two, so the
/// product is exact.
pub const RIGHT_GAIN: f32 = 0.5;

/// Length of the fade in and the fade out.
pub const FADE_SECONDS: f32 = 0.005;

const VOICES: usize = 16;

/// The pitch of a MIDI key, with A4 (key 69) at 440 Hz.
pub fn key_frequency(key: u8) -> f64 {
    440.0 * 2f64.powf((f64::from(key) - 69.0) / 12.0)
}

pub struct TestSynth;

/// The CLAP entry type of this plugin, for loading it in-process.
pub type Entry = SinglePluginEntry<TestSynth>;

impl Plugin for TestSynth {
    type AudioProcessor<'a> = SynthProcessor;
    type Shared<'a> = SynthShared;
    type MainThread<'a> = SynthMainThread;

    fn declare_extensions(builder: &mut PluginExtensions<Self>, _shared: Option<&SynthShared>) {
        builder
            .register::<PluginAudioPorts>()
            .register::<PluginNotePorts>();
    }
}

impl DefaultPluginFactory for TestSynth {
    fn get_descriptor() -> PluginDescriptor {
        use clack_plugin::plugin::features::{INSTRUMENT, STEREO, SYNTHESIZER};
        PluginDescriptor::new(ID, "refrain test synth").with_features([
            INSTRUMENT,
            SYNTHESIZER,
            STEREO,
        ])
    }

    fn new_shared(_host: HostSharedHandle) -> Result<SynthShared, PluginError> {
        Ok(SynthShared)
    }

    fn new_main_thread<'a>(
        _host: HostMainThreadHandle<'a>,
        _shared: &'a SynthShared,
    ) -> Result<SynthMainThread, PluginError> {
        Ok(SynthMainThread)
    }
}

pub struct SynthShared;

impl PluginShared<'_> for SynthShared {}

pub struct SynthMainThread;

impl PluginMainThread<'_, SynthShared> for SynthMainThread {}

#[derive(Clone, Copy, Default)]
struct Voice {
    active: bool,
    channel: u16,
    key: u16,
    // Kept in [0, 1) and in f64, so the pitch does not drift.
    phase: f64,
    step: f64,
    gain: f32,
    level: f32,
    releasing: bool,
}

pub struct SynthProcessor {
    voices: [Voice; VOICES],
    sample_rate: f64,
    fade_step: f32,
}

impl SynthProcessor {
    fn note_on(&mut self, channel: u16, key: u16, velocity: f64) {
        // Reuse a free voice. When all are busy, the new note is dropped.
        let Some(voice) = self.voices.iter_mut().find(|v| !v.active) else {
            return;
        };
        *voice = Voice {
            active: true,
            channel,
            key,
            phase: 0.0,
            step: key_frequency(key.min(127) as u8) / self.sample_rate,
            gain: velocity as f32 * PEAK,
            level: 0.0,
            releasing: false,
        };
    }

    fn note_off(&mut self, channel: Match<u16>, key: Match<u16>) {
        for v in self.voices.iter_mut().filter(|v| v.active) {
            if channel.matches(v.channel) && key.matches(v.key) {
                v.releasing = true;
            }
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
            _ => {}
        }
    }

    fn render(&mut self, out: &mut [f32]) {
        for v in self.voices.iter_mut().filter(|v| v.active) {
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
                v.phase += v.step;
                v.phase -= v.phase.floor();
            }
        }
    }
}

impl<'a> PluginAudioProcessor<'a, SynthShared, SynthMainThread> for SynthProcessor {
    fn activate(
        _host: HostAudioProcessorHandle<'a>,
        _main_thread: &SynthMainThread,
        _shared: &'a SynthShared,
        config: PluginAudioConfiguration,
    ) -> Result<Self, PluginError> {
        Ok(Self {
            voices: [Voice::default(); VOICES],
            sample_rate: config.sample_rate,
            fade_step: 1.0 / (FADE_SECONDS * config.sample_rate as f32),
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

impl PluginAudioPortsImpl for SynthMainThread {
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

impl PluginNotePortsImpl for SynthMainThread {
    fn count(&self, is_input: bool) -> u32 {
        if is_input { 1 } else { 0 }
    }

    fn get(&self, index: u32, is_input: bool, writer: &mut NotePortInfoWriter) {
        if is_input && index == 0 {
            writer.set(&NotePortInfo {
                id: ClapId::new(0),
                name: b"notes",
                preferred_dialect: Some(NoteDialect::Clap),
                supported_dialects: NoteDialects::CLAP | NoteDialects::MIDI,
            });
        }
    }
}

clack_export_entry!(Entry);
