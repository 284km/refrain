//! The `refrain` command.
//!
//! For now this is a small command-line front end that proves the engine runs
//! on a real device and offline. The GUI comes later.

use std::error::Error;
use std::process::ExitCode;
use std::time::Duration;

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{FromSample, SampleFormat, SizedSample};
use refrain_engine::{Processor, Sine, render_offline};

type Result<T> = std::result::Result<T, Box<dyn Error>>;

const USAGE: &str = "\
usage:
  refrain devices                         list output devices
  refrain sine [options]                  play a sine wave on a device
  refrain render <out.wav> [options]      write a sine wave to a WAV file

options:
  --freq <hz>        frequency (default 440)
  --seconds <s>      length (default 2)
  --amp <0..1>       amplitude (default 0.2)
  --device <id>      output device id, as printed by `refrain devices` (sine)
  --rate <hz>        sample rate (render, default 48000)
  --channels <n>     channel count (render, default 2)";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let result = match args.first().map(String::as_str) {
        Some("devices") => devices(),
        Some("sine") => Options::parse(&args[1..]).and_then(|o| sine(&o)),
        Some("render") => match args.get(1) {
            Some(path) if !path.starts_with("--") => {
                Options::parse(&args[2..]).and_then(|o| render(path, &o))
            }
            _ => Err("render needs an output path".into()),
        },
        Some("-h" | "--help" | "help") => {
            println!("{USAGE}");
            Ok(())
        }
        _ => Err("unknown command".into()),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("refrain: {e}\n\n{USAGE}");
            ExitCode::FAILURE
        }
    }
}

struct Options {
    freq: f64,
    seconds: f64,
    amp: f32,
    device: Option<String>,
    rate: u32,
    channels: u16,
}

impl Options {
    fn parse(args: &[String]) -> Result<Self> {
        let mut o = Options {
            freq: 440.0,
            seconds: 2.0,
            amp: 0.2,
            device: None,
            rate: 48_000,
            channels: 2,
        };
        let mut it = args.iter();
        while let Some(flag) = it.next() {
            let value = it.next().ok_or_else(|| format!("{flag} needs a value"))?;
            match flag.as_str() {
                "--freq" => o.freq = value.parse()?,
                "--seconds" => o.seconds = value.parse()?,
                "--amp" => o.amp = value.parse()?,
                "--device" => o.device = Some(value.clone()),
                "--rate" => o.rate = value.parse()?,
                "--channels" => o.channels = value.parse()?,
                _ => return Err(format!("unknown option {flag}").into()),
            }
        }
        if !(0.0..=1.0).contains(&o.amp) {
            return Err("--amp must be between 0 and 1".into());
        }
        if o.channels == 0 || o.rate == 0 {
            return Err("--rate and --channels must be positive".into());
        }
        Ok(o)
    }
}

fn devices() -> Result<()> {
    let host = cpal::default_host();
    println!("host: {}", host.id().name());
    let default_id = host.default_output_device().and_then(|d| d.id().ok());
    for device in host.output_devices()? {
        let id = device.id()?;
        let mark = if Some(&id) == default_id.as_ref() {
            "*"
        } else {
            " "
        };
        let name = device
            .description()
            .map(|d| d.to_string())
            .unwrap_or_default();
        println!("{mark} {id}  {name}");
        if let Ok(config) = device.default_output_config() {
            println!(
                "    {} ch, {} Hz, {}",
                config.channels(),
                config.sample_rate(),
                config.sample_format()
            );
        }
    }
    Ok(())
}

fn sine(o: &Options) -> Result<()> {
    let host = cpal::default_host();
    let device = match &o.device {
        Some(id) => host.device_by_id(&id.parse()?),
        None => host.default_output_device(),
    }
    .ok_or("no output device")?;
    let config = device.default_output_config()?;
    println!(
        "playing {} Hz on {} ({} ch, {} Hz, {})",
        o.freq,
        device.id()?,
        config.channels(),
        config.sample_rate(),
        config.sample_format()
    );
    let sine = Sine::new(o.freq, o.amp);
    let stream = match config.sample_format() {
        SampleFormat::F32 => build::<f32>(&device, config.into(), sine)?,
        SampleFormat::I16 => build::<i16>(&device, config.into(), sine)?,
        SampleFormat::I32 => build::<i32>(&device, config.into(), sine)?,
        SampleFormat::U16 => build::<u16>(&device, config.into(), sine)?,
        other => return Err(format!("sample format {other} is not supported yet").into()),
    };
    stream.play()?;
    std::thread::sleep(Duration::from_secs_f64(o.seconds));
    Ok(())
}

/// Opens an output stream that runs `processor` in the device callback.
fn build<T>(
    device: &cpal::Device,
    config: cpal::StreamConfig,
    mut processor: impl Processor + 'static,
) -> Result<cpal::Stream>
where
    T: SizedSample + FromSample<f32>,
{
    let channels = usize::from(config.channels);
    processor.prepare(config.sample_rate as f32);
    // The device may use a sample type other than f32. The engine still works
    // in f32, through this buffer, which lives in the callback so the audio
    // thread never allocates.
    let mut scratch = [0.0_f32; 4096];
    let chunk = scratch.len() / channels * channels;
    let stream = device.build_output_stream(
        config,
        move |data: &mut [T], _: &cpal::OutputCallbackInfo| {
            for out in data.chunks_mut(chunk) {
                let tmp = &mut scratch[..out.len()];
                processor.process(tmp, channels);
                for (o, &s) in out.iter_mut().zip(tmp.iter()) {
                    *o = T::from_sample(s);
                }
            }
        },
        |err| eprintln!("stream error: {err}"),
        None,
    )?;
    Ok(stream)
}

fn render(path: &str, o: &Options) -> Result<()> {
    let frames = (o.seconds * f64::from(o.rate)).round() as usize;
    let mut sine = Sine::new(o.freq, o.amp);
    let samples = render_offline(
        &mut sine,
        o.rate as f32,
        usize::from(o.channels),
        frames,
        128,
    );
    let spec = hound::WavSpec {
        channels: o.channels,
        sample_rate: o.rate,
        bits_per_sample: 32,
        sample_format: hound::SampleFormat::Float,
    };
    let mut writer = hound::WavWriter::create(path, spec)?;
    for s in samples {
        writer.write_sample(s)?;
    }
    writer.finalize()?;
    println!("wrote {frames} frames to {path}");
    Ok(())
}
