//! The `refrain` command.
//!
//! For now this is a command-line front end that proves the engine and plugin
//! hosting work on a real device and offline. The GUI comes later.

mod audio;
mod editor;
mod midi;
mod plugin;

use std::error::Error;
use std::io::BufRead;
use std::path::Path;
use std::process::ExitCode;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use cpal::traits::{DeviceTrait, HostTrait};
use refrain_engine::{Event, Processor, Recording, SUSTAIN, Sequencer, Sine, render_offline};

use audio::Output;
use plugin::Plugin;

type Result<T> = std::result::Result<T, Box<dyn Error>>;

const USAGE: &str = "\
usage:
  refrain devices                       list audio output devices
  refrain midi                          list MIDI inputs
  refrain plugins                       list installed CLAP and VST3 plugins
  refrain sine [options]                play a sine wave
  refrain play <plugin> [options]       play a .clap or .vst3 instrument from MIDI,
                                        with its editor open. A .refrain project
                                        works too, here and below
  refrain record <plugin> [options]     like play, and record what comes in from MIDI
                                        as a new clip; needs --save, or a project
  refrain editor <plugin> [options]     open a plugin's editor, without audio
  refrain render <out.wav> [options]    write a sine wave, or with --plugin a
                                        project's clips or else a short phrase, to
                                        a WAV file

options:
  --device <id>      audio output, as `refrain devices` prints it
  --midi <n|name>    MIDI input: its number in `refrain midi`, or part of its name
  --id <id|name>     which plugin, when a file holds several (VST3: name or class ID)
  --plugin <path>    render through this .clap or .vst3 plugin
  --demo             play the short phrase instead of reading MIDI (play)
  --no-editor        do not open the plugin's editor (play)
  --save <project>   when done, save the plugin, its state and its clips as a
                     project (play, record, editor)
  --tempo <bpm>      beats per minute (default: the project's, or 120)
  --click            play a metronome (play; record plays one unless --no-click)
  --seconds <s>      length (default 2; play and editor run until closed without it)
  --freq <hz>        sine frequency (default 440)
  --amp <0..1>       sine amplitude (default 0.2)
  --rate <hz>        sample rate (render, default 48000)
  --channels <n>     channel count (render, default 2)

Loading a plugin runs its code in this process.";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let with_path = |f: fn(&str, &Options) -> Result<()>, what: &str| match args.get(1) {
        Some(path) if !path.starts_with("--") => {
            Options::parse(&args[2..]).and_then(|o| f(path, &o))
        }
        _ => Err(format!("{what} needs a path").into()),
    };
    let result = match args.first().map(String::as_str) {
        Some("devices") => devices(),
        Some("midi") => list_midi(),
        Some("plugins") => list_plugins(),
        Some("sine") => Options::parse(&args[1..]).and_then(|o| sine(&o)),
        Some("play") => with_path(play, "play"),
        Some("record") => with_path(record, "record"),
        Some("editor") => with_path(editor_only, "editor"),
        Some("render") => with_path(render, "render"),
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
    device: Option<String>,
    midi: Option<String>,
    id: Option<String>,
    plugin: Option<String>,
    save: Option<String>,
    demo: bool,
    no_editor: bool,
    tempo: Option<f64>,
    click: bool,
    no_click: bool,
    seconds: Option<f64>,
    freq: f64,
    amp: f32,
    rate: u32,
    channels: u16,
}

impl Options {
    fn parse(args: &[String]) -> Result<Self> {
        let mut o = Options {
            device: None,
            midi: None,
            id: None,
            plugin: None,
            save: None,
            demo: false,
            no_editor: false,
            tempo: None,
            click: false,
            no_click: false,
            seconds: None,
            freq: 440.0,
            amp: 0.2,
            rate: 48_000,
            channels: 2,
        };
        let mut it = args.iter();
        while let Some(flag) = it.next() {
            if flag == "--demo" {
                o.demo = true;
                continue;
            }
            if flag == "--no-editor" {
                o.no_editor = true;
                continue;
            }
            if flag == "--click" {
                o.click = true;
                continue;
            }
            if flag == "--no-click" {
                o.no_click = true;
                continue;
            }
            let value = it.next().ok_or_else(|| format!("{flag} needs a value"))?;
            match flag.as_str() {
                "--device" => o.device = Some(value.clone()),
                "--midi" => o.midi = Some(value.clone()),
                "--id" => o.id = Some(value.clone()),
                "--plugin" => o.plugin = Some(value.clone()),
                "--save" => o.save = Some(value.clone()),
                "--tempo" => o.tempo = Some(value.parse()?),
                "--seconds" => o.seconds = Some(value.parse()?),
                "--freq" => o.freq = value.parse()?,
                "--amp" => o.amp = value.parse()?,
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
        if o.tempo.is_some_and(|t| !(1.0..=1_000.0).contains(&t)) {
            return Err("--tempo must be between 1 and 1000".into());
        }
        if o.seconds.is_some_and(|s| s.is_nan() || s < 0.0) {
            return Err("--seconds must not be negative".into());
        }
        Ok(o)
    }
}

/// A short phrase for trying an instrument without a keyboard: C, E, G and C
/// one after another with the sustain pedal down, so they ring together, then
/// the C major chord without it. Times are in seconds.
/// The demo's velocity: a MIDI value, so a recorded demo plays back exactly.
const DEMO_VELOCITY: f32 = 100.0 / 127.0;

fn demo_phrase() -> Vec<(f64, Event)> {
    let mut phrase = vec![
        (
            0.0,
            Event::Controller {
                channel: 0,
                number: SUSTAIN,
                value: 127,
            },
        ),
        (
            0.95,
            Event::Controller {
                channel: 0,
                number: SUSTAIN,
                value: 0,
            },
        ),
    ];
    let mut note = |start: f64, end: f64, key: u8| {
        phrase.push((
            start,
            Event::NoteOn {
                channel: 0,
                key,
                velocity: DEMO_VELOCITY,
            },
        ));
        phrase.push((
            end,
            Event::NoteOff {
                channel: 0,
                key,
                velocity: DEMO_VELOCITY,
            },
        ));
    };
    for (i, key) in [60, 64, 67, 72].into_iter().enumerate() {
        let start = i as f64 * 0.2;
        note(start, start + 0.15, key);
    }
    for key in [60, 64, 67] {
        note(1.0, 1.8, key);
    }
    phrase.sort_by(|a, b| a.0.total_cmp(&b.0));
    phrase
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

fn list_midi() -> Result<()> {
    let names = midi::port_names()?;
    if names.is_empty() {
        println!("no MIDI inputs");
    }
    for (i, name) in names.iter().enumerate() {
        println!("{i}  {name}");
    }
    Ok(())
}

fn list_plugins() -> Result<()> {
    plugin::list_installed();
    Ok(())
}

fn sine(o: &Options) -> Result<()> {
    let output = Output::open(o.device.as_deref())?;
    println!("playing {} Hz on {}", o.freq, output.describe());
    let _stream = output.start(Sine::new(o.freq, o.amp), None)?;
    std::thread::sleep(Duration::from_secs_f64(o.seconds.unwrap_or(2.0)));
    Ok(())
}

fn play(path: &str, o: &Options) -> Result<()> {
    perform(path, o, false)
}

fn record(path: &str, o: &Options) -> Result<()> {
    perform(path, o, true)
}

/// Plays an instrument, its project's clips, and what comes in live; and,
/// when `recording`, records what comes in live as a new clip.
fn perform(path: &str, o: &Options, recording: bool) -> Result<()> {
    let is_project = Path::new(path)
        .extension()
        .is_some_and(|e| e.eq_ignore_ascii_case(refrain_project::EXTENSION));
    // A recording is never thrown away: it goes back into the project it
    // was made on, unless --save says where.
    let save = match (&o.save, recording) {
        (Some(save), _) => Some(save.clone()),
        (None, true) if is_project => Some(path.to_owned()),
        (None, true) => return Err("record needs --save <project> to keep the recording in".into()),
        (None, false) => None,
    };

    // SAFETY: the user asked for this file to be loaded.
    let mut plugin = unsafe { Plugin::open(Path::new(path), o.id.as_deref()) }?;
    if let Some(bpm) = o.tempo {
        plugin.set_tempo(bpm);
    }
    let tempo = plugin.tempo();
    let output = Output::open(o.device.as_deref())?;
    let rate = output.sample_rate();
    let processor = plugin.activate(rate, refrain_engine::MAX_BLOCK_FRAMES)?;
    let mut sequencer = Sequencer::new(processor, plugin.sequence(rate));
    let click = if recording { !o.no_click } else { o.click };
    if click {
        sequencer = sequencer.with_click(tempo, 4);
    }
    let plugin_title = plugin.describe();
    println!("{plugin_title}");
    println!("output: {}", output.describe());
    if click {
        println!("click at {} beats per minute", tempo.beats_per_minute);
    }

    // Events reach the audio thread through this queue, from the MIDI thread
    // or, with --demo, from the loop below. When recording, they also come
    // to the loop below, with the moment they arrived.
    let (to_audio, from_notes) = rtrb::RingBuffer::new(1024);
    let (to_recording, mut from_midi) = rtrb::RingBuffer::new(4096);
    let mut demo = Vec::new();
    let mut demo_events = None;
    let mut _midi = None;
    let mut seconds = o.seconds;
    if o.demo {
        demo = demo_phrase();
        let end = demo.last().map_or(0.0, |e| e.0);
        seconds.get_or_insert(end + 0.5);
        demo_events = Some(to_audio);
        println!("playing the demo phrase");
    } else {
        _midi = midi::connect(
            o.midi.as_deref(),
            to_audio,
            recording.then_some(to_recording),
        )?;
        match &_midi {
            Some(connection) => println!("MIDI input: {}", connection.port_name),
            None if recording => return Err("there is no MIDI input to record from".into()),
            None => println!("no MIDI input; try --demo"),
        }
    }

    let stream = output.start(sequencer, Some(from_notes))?;
    // The timeline and the recording both start now.
    let start = Instant::now();
    let stop = Arc::new(AtomicBool::new(false));
    if seconds.is_none() {
        println!(
            "{} press Enter, or close the editor, to stop",
            if recording { "recording;" } else { "playing;" }
        );
        let stop = Arc::clone(&stop);
        std::thread::spawn(move || {
            let _ = std::io::stdin().lock().read_line(&mut String::new());
            stop.store(true, Ordering::Release);
        });
    }

    // One turn of the main thread: plays due demo events, collects what is
    // being recorded, and answers the plugin. Returns false when it is time
    // to stop.
    let mut taken = Recording::default();
    let mut demo = demo.into_iter().peekable();
    let mut tick = |plugin: &mut Plugin| {
        let elapsed = start.elapsed().as_secs_f64();
        if stop.load(Ordering::Acquire) || seconds.is_some_and(|s| elapsed >= s) {
            return false;
        }
        while let Some((at, event)) = demo.next_if(|(t, _)| *t <= elapsed) {
            if let Some(tx) = demo_events.as_mut() {
                let _ = tx.push(event);
            }
            if recording {
                // The demo is recorded at the moments it was meant for.
                taken.add(at, event);
            }
        }
        while let Ok((at, event)) = from_midi.pop() {
            taken.add(at.saturating_duration_since(start).as_secs_f64(), event);
        }
        plugin.poll_main_thread();
        true
    };

    let shown = !o.no_editor && editor::run(&mut plugin, &plugin_title, &mut tick)?;
    if !o.no_editor && !shown {
        println!("the plugin has no editor to show");
    }
    if !shown {
        while tick(&mut plugin) {
            std::thread::sleep(Duration::from_millis(5));
        }
    }
    let stopped = start.elapsed().as_secs_f64();
    while let Ok((at, event)) = from_midi.pop() {
        taken.add(at.saturating_duration_since(start).as_secs_f64(), event);
    }

    if recording {
        if taken.is_empty() {
            println!("nothing was played; the project keeps its clips as they were");
        } else {
            let events: Vec<(u64, Event)> = taken
                .finish(stopped)
                .into_iter()
                .map(|(at, event)| (tempo.ticks_at(at), event))
                .collect();
            println!("recorded {} events into a new clip", events.len());
            plugin.add_clip(&events);
        }
    }
    if let Some(project) = &save {
        plugin.save_project(Path::new(project))?;
        println!("saved {project}");
    }

    // The stream owns the plugin's processor. It has to stop before the
    // plugin is destroyed.
    drop(stream);
    drop(plugin);
    Ok(())
}

fn editor_only(path: &str, o: &Options) -> Result<()> {
    // SAFETY: the user asked for this file to be loaded.
    let mut plugin = unsafe { Plugin::open(Path::new(path), o.id.as_deref()) }?;
    let title = plugin.describe();
    println!("{title}");
    let start = Instant::now();
    let mut tick = |plugin: &mut Plugin| {
        plugin.poll_main_thread();
        o.seconds.is_none_or(|s| start.elapsed().as_secs_f64() < s)
    };
    if !editor::run(&mut plugin, &title, &mut tick)? {
        return Err("the plugin has no editor to show".into());
    }
    if let Some(project) = &o.save {
        plugin.save_project(Path::new(project))?;
        println!("saved {project}");
    }
    Ok(())
}

fn render(path: &str, o: &Options) -> Result<()> {
    let frames = (o.seconds.unwrap_or(2.0) * f64::from(o.rate)).round() as usize;
    let rate = o.rate as f32;
    let channels = usize::from(o.channels);
    let block = 128;

    let samples = match &o.plugin {
        None => render_offline(
            &mut Sine::new(o.freq, o.amp),
            rate,
            channels,
            frames,
            block,
            &[],
        ),
        Some(plugin_path) => {
            // SAFETY: the user asked for this file to be loaded.
            let mut plugin = unsafe { Plugin::open(Path::new(plugin_path), o.id.as_deref()) }?;
            println!("{}", plugin.describe());
            if let Some(bpm) = o.tempo {
                plugin.set_tempo(bpm);
            }
            let processor = plugin.activate(rate, block)?;
            // A project's clips, or else the demo phrase.
            let mut sequence = plugin.sequence(rate);
            let mut frames = frames;
            if sequence.is_empty() {
                sequence = demo_phrase()
                    .into_iter()
                    .map(|(t, event)| ((t * f64::from(o.rate)).round() as u64, event))
                    .collect();
            } else if o.seconds.is_none() {
                // The clips, and a second for the sound to die away.
                frames = plugin.sequence_end(rate) as usize + o.rate as usize;
            }
            let mut processor = Sequencer::new(processor, sequence);
            let out = render_offline(&mut processor, rate, channels, frames, block, &[]);
            if processor.has_failed() {
                return Err("the plugin failed while processing".into());
            }
            drop(processor);
            out
        }
    };

    let peak = samples.iter().fold(0.0_f32, |m, s| m.max(s.abs()));
    let written = samples.len() / channels;
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
    println!("wrote {written} frames to {path}, peak {peak:.3}");
    Ok(())
}
