//! Runs a [`Processor`] on an audio output device.

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{FromSample, SampleFormat, SizedSample};
use refrain_engine::{MAX_BLOCK_EVENTS, MAX_BLOCK_FRAMES, NoteEvent, Processor, TimedEvent};

use crate::Result;

/// An output device and the configuration it prefers.
pub struct Output {
    pub device: cpal::Device,
    pub config: cpal::SupportedStreamConfig,
}

impl Output {
    /// Opens the device with `id`, or the default output device.
    pub fn open(id: Option<&str>) -> Result<Self> {
        let host = cpal::default_host();
        let device = match id {
            Some(id) => host.device_by_id(&id.parse()?),
            None => host.default_output_device(),
        }
        .ok_or("no output device")?;
        let config = device.default_output_config()?;
        Ok(Self { device, config })
    }

    pub fn sample_rate(&self) -> f32 {
        self.config.sample_rate() as f32
    }

    pub fn describe(&self) -> String {
        let id = self
            .device
            .id()
            .map(|id| id.to_string())
            .unwrap_or_default();
        format!(
            "{id} ({} ch, {} Hz, {})",
            self.config.channels(),
            self.config.sample_rate(),
            self.config.sample_format()
        )
    }

    /// Starts a stream that runs `processor`. Notes from `events`, if given,
    /// are passed to it at the start of the next block.
    pub fn start(
        &self,
        processor: impl Processor + 'static,
        events: Option<rtrb::Consumer<NoteEvent>>,
    ) -> Result<cpal::Stream> {
        let config: cpal::StreamConfig = self.config.into();
        let stream = match self.config.sample_format() {
            SampleFormat::F32 => build::<f32>(&self.device, config, processor, events)?,
            SampleFormat::I16 => build::<i16>(&self.device, config, processor, events)?,
            SampleFormat::I32 => build::<i32>(&self.device, config, processor, events)?,
            SampleFormat::U16 => build::<u16>(&self.device, config, processor, events)?,
            other => return Err(format!("sample format {other} is not supported yet").into()),
        };
        stream.play()?;
        Ok(stream)
    }
}

fn build<T>(
    device: &cpal::Device,
    config: cpal::StreamConfig,
    mut processor: impl Processor + 'static,
    mut events: Option<rtrb::Consumer<NoteEvent>>,
) -> Result<cpal::Stream>
where
    T: SizedSample + FromSample<f32>,
{
    let channels = usize::from(config.channels);
    processor.prepare(config.sample_rate as f32, MAX_BLOCK_FRAMES);
    // Everything the callback needs is allocated here, before the stream
    // starts, so the audio thread never allocates. The engine works in f32;
    // `scratch` holds a block before it is converted to the device's type.
    let mut scratch = vec![0.0_f32; MAX_BLOCK_FRAMES * channels];
    let mut block_events = Vec::with_capacity(MAX_BLOCK_EVENTS);
    let stream = device.build_output_stream(
        config,
        move |data: &mut [T], _: &cpal::OutputCallbackInfo| {
            for out in data.chunks_mut(MAX_BLOCK_FRAMES * channels) {
                block_events.clear();
                if let Some(rx) = events.as_mut() {
                    // Notes are not timestamped yet: each one starts at the
                    // beginning of the block after it arrives.
                    while block_events.len() < MAX_BLOCK_EVENTS {
                        let Ok(event) = rx.pop() else { break };
                        block_events.push(TimedEvent { frame: 0, event });
                    }
                }
                let block = &mut scratch[..out.len()];
                processor.process(block, channels, &block_events);
                for (o, &s) in out.iter_mut().zip(block.iter()) {
                    *o = T::from_sample(s);
                }
            }
        },
        |err| eprintln!("stream error: {err}"),
        None,
    )?;
    Ok(stream)
}
