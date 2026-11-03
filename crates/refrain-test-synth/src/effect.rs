//! The CLAP test effect: [`refrain_test_tone::effect`], with a stereo input
//! and output and its gain as its state. It takes no notes and has no editor.

use std::io::{Read, Write};
use std::sync::atomic::{AtomicU32, Ordering};

use clack_extensions::audio_ports::{
    AudioPortFlags, AudioPortInfo, AudioPortInfoWriter, AudioPortType, PluginAudioPorts,
    PluginAudioPortsImpl,
};
use clack_extensions::render::{PluginRender, PluginRenderImpl, RenderMode};
use clack_extensions::state::{PluginState, PluginStateImpl};
use clack_plugin::prelude::*;
use clack_plugin::process::audio::ChannelPair;
use clack_plugin::stream::{InputStream, OutputStream};
use refrain_test_tone::effect;

pub struct TestEffect;

impl Plugin for TestEffect {
    type AudioProcessor<'a> = EffectProcessor<'a>;
    type Shared<'a> = EffectShared;
    type MainThread<'a> = EffectMainThread<'a>;

    fn declare_extensions(builder: &mut PluginExtensions<Self>, _shared: Option<&EffectShared>) {
        builder
            .register::<PluginAudioPorts>()
            .register::<PluginState>()
            .register::<PluginRender>();
    }
}

pub struct EffectShared {
    // The gain, as f32 bits: set by the main thread from a saved state.
    gain: AtomicU32,
}

impl EffectShared {
    pub fn new() -> Self {
        Self {
            gain: AtomicU32::new(effect::DEFAULT_GAIN.to_bits()),
        }
    }

    fn gain(&self) -> f32 {
        f32::from_bits(self.gain.load(Ordering::Relaxed))
    }
}

impl PluginShared<'_> for EffectShared {}

pub struct EffectMainThread<'a> {
    pub shared: &'a EffectShared,
}

impl<'a> PluginMainThread<'a, EffectShared> for EffectMainThread<'a> {}

pub struct EffectProcessor<'a> {
    shared: &'a EffectShared,
}

impl<'a> PluginAudioProcessor<'a, EffectShared, EffectMainThread<'a>> for EffectProcessor<'a> {
    fn activate(
        _host: HostAudioProcessorHandle<'a>,
        _main_thread: &EffectMainThread,
        shared: &'a EffectShared,
        _config: PluginAudioConfiguration,
    ) -> Result<Self, PluginError> {
        Ok(Self { shared })
    }

    fn process(
        &mut self,
        _process: Process,
        mut audio: Audio,
        _events: Events,
    ) -> Result<ProcessStatus, PluginError> {
        let gain = self.shared.gain();
        let mut port = audio
            .port_pair(0)
            .ok_or(PluginError::Message("no audio port"))?;
        let channels = port
            .channels()?
            .into_f32()
            .ok_or(PluginError::Message("expected f32 audio"))?;
        for pair in channels {
            match pair {
                ChannelPair::InputOutput(input, output) => {
                    output.copy_from_slice(input);
                    effect::process(gain, output);
                }
                ChannelPair::InPlace(buffer) => effect::process(gain, buffer),
                ChannelPair::OutputOnly(output) => output.fill(0.0),
                ChannelPair::InputOnly(_) => {}
            }
        }
        Ok(ProcessStatus::ContinueIfNotQuiet)
    }
}

impl PluginAudioPortsImpl for EffectMainThread<'_> {
    fn count(&self, _is_input: bool) -> u32 {
        1
    }

    fn get(&self, index: u32, is_input: bool, writer: &mut AudioPortInfoWriter) {
        if index == 0 {
            writer.set(&AudioPortInfo {
                id: ClapId::new(0),
                name: if is_input { b"in" } else { b"out" },
                channel_count: 2,
                flags: AudioPortFlags::IS_MAIN,
                port_type: Some(AudioPortType::STEREO),
                in_place_pair: Some(ClapId::new(0)),
            });
        }
    }
}

impl PluginRenderImpl for EffectMainThread<'_> {
    fn has_hard_realtime_requirement(&self) -> bool {
        false
    }

    fn set(&self, mode: RenderMode) -> Result<(), PluginError> {
        crate::set_render_mode(mode)
    }
}

/// The saved state: the gain, in the format shared with the VST3 effect.
impl PluginStateImpl for EffectMainThread<'_> {
    fn save(&self, output: &mut OutputStream) -> Result<(), PluginError> {
        output
            .write_all(&effect::save_state(self.shared.gain()))
            .map_err(|_| PluginError::Message("could not write the state"))
    }

    fn load(&self, input: &mut InputStream) -> Result<(), PluginError> {
        let mut state = Vec::new();
        input
            .read_to_end(&mut state)
            .map_err(|_| PluginError::Message("could not read the state"))?;
        let gain =
            effect::load_state(&state).ok_or(PluginError::Message("not a state of this plugin"))?;
        self.shared.gain.store(gain.to_bits(), Ordering::Relaxed);
        Ok(())
    }
}
