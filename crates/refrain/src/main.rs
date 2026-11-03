//! The `refrain` command.
//!
//! Commands that play, record, render and change projects, and `refrain gui`,
//! which opens refrain's own window.

mod audio;
mod editor;
mod gui;
mod midi;
mod plugin;
mod project;
mod session;
mod wav;

use std::error::Error;
use std::io::BufRead;
use std::path::Path;
use std::process::ExitCode;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use cpal::traits::{DeviceTrait, HostTrait};
use refrain_engine::{
    Event, Processor, Recording, SUSTAIN, Sine, Tail, render_offline, render_until_quiet,
};

use audio::Output;
use session::{Session, Slot};

type Result<T> = std::result::Result<T, Box<dyn Error>>;

const USAGE: &str = "\
usage:
  refrain devices                       list audio output devices
  refrain midi                          list MIDI inputs
  refrain plugins                       list installed CLAP and VST3 plugins
  refrain sine [options]                play a sine wave
  refrain play <plugin> [options]       play a .clap or .vst3 instrument from MIDI,
                                        with its editor open. A .refrain project
                                        works too, here and below, and plays all
                                        its tracks; MIDI goes to one, --track
  refrain record <plugin> [options]     like play, and record what comes in from MIDI
                                        as a new clip; needs --save, or a project
  refrain editor <plugin> [options]     open a plugin's editor, without audio
  refrain gui [project] [options]       open refrain's window on a project, on a
                                        plugin, or on a new project; --save says
                                        where to save
  refrain render <out.wav> [options]    write a sine wave, or with --plugin a
                                        project's clips or else a short phrase, to
                                        a WAV file: until the sound dies away, and
                                        with plugins told it is not real time

projects:
  refrain new <project>                 make an empty project
  refrain show <project>                list its tracks, effects and mix
  refrain track add <project> <plugin>  add a track playing an instrument; from a
                                        .refrain, add that project's tracks
  refrain track remove <project> <n>    remove track n
  refrain fx add <project> <plugin>     add an effect at the end of a track's chain
                                        (--track) or the master's (--master)
  refrain fx remove <project> <k>       remove effect k of a track or the master
  refrain mix <project> [options]       set a track's or the master's mix:
                                        --volume, --pan, --mute, --solo

options:
  --device <id>      audio output, as `refrain devices` prints it
  --midi <n|name>    MIDI input: its number in `refrain midi`, or part of its name
  --id <id|name>     which plugin, when a file holds several (VST3: name or class ID)
  --plugin <path>    render through this .clap or .vst3 plugin or .refrain project
  --track <n>        which track, counted from 1 (default 1): MIDI and recording
                     go to it, and its plugins' editors open
  --fx <k>           open the editor of effect k of the track, or of the master
  --master           the master instead of a track (fx, mix, and with --fx)
  --name <name>      the new track's name (track add)
  --volume <dB>      volume in decibels (mix)
  --pan <-1..1>      balance from left to right (mix)
  --mute, --unmute, --solo, --unsolo
                     mute a track, or solo it (mix)
  --demo             play the short phrase instead of reading MIDI (play)
  --no-editor        do not open the plugin's editor (play)
  --play             start playing as soon as the window opens (gui)
  --save <project>   when done, save the project with every plugin's state and
                     the clips (play, record, editor)
  --tempo <bpm>      beats per minute (default: the project's, or 120; new)
  --click            play a metronome (play; record plays one unless --no-click)
  --seconds <s>      length (default: render waits for the sound to die away, a
                     sine lasts 2; play and editor run until closed)
  --freq <hz>        sine frequency (default 440)
  --amp <0..1>       sine amplitude (default 0.2)
  --rate <hz>        sample rate (render, default 48000)
  --channels <n>     channel count (render, default 2)
  --bits <16|24|32>  sample size: 16- or 24-bit integers, or 32-bit float (render,
                     default 32)
  --dither           add triangular dither to a 16- or 24-bit file (render)

Loading a plugin runs its code in this process.";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let result = run(&args);
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) if e.is::<Usage>() => {
            eprintln!("refrain: {e}\n\n{USAGE}");
            ExitCode::FAILURE
        }
        Err(e) => {
            eprintln!("refrain: {e}");
            ExitCode::FAILURE
        }
    }
}

/// A command given wrongly. Only these errors come with the usage text.
#[derive(Debug)]
struct Usage(String);

impl std::fmt::Display for Usage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl Error for Usage {}

fn usage(e: impl std::fmt::Display) -> Box<dyn Error> {
    Box::new(Usage(e.to_string()))
}

/// Runs a command: its words and paths first, then its options.
fn run(args: &[String]) -> Result<()> {
    let options_start = args
        .iter()
        .position(|a| a.starts_with("--"))
        .unwrap_or(args.len());
    let (words, flags) = args.split_at(options_start);
    let words: Vec<&str> = words.iter().map(String::as_str).collect();
    if matches!(words.as_slice(), [] | ["help"]) && flags.iter().any(|f| f == "--help") {
        println!("{USAGE}");
        return Ok(());
    }
    let o = Options::parse(flags).map_err(usage)?;
    match words.as_slice() {
        ["help"] | ["-h"] => {
            println!("{USAGE}");
            Ok(())
        }
        ["devices"] => devices(),
        ["midi"] => list_midi(),
        ["plugins"] => list_plugins(),
        ["sine"] => sine(&o),
        ["play", path] => play(path, &o),
        ["record", path] => record(path, &o),
        ["editor", path] => editor_only(path, &o),
        ["render", path] => render(path, &o),
        ["gui"] => gui::run(None, &o),
        ["gui", path] => gui::run(Some(Path::new(path)), &o),
        ["new", path] => project::new(Path::new(path), &o),
        ["show", path] => project::show(Path::new(path)),
        ["track", "add", path, plugin] => {
            project::add_track(Path::new(path), Path::new(plugin), &o)
        }
        ["track", "remove", path, n] => project::remove_track(Path::new(path), n),
        ["fx", "add", path, plugin] => project::add_effect(Path::new(path), Path::new(plugin), &o),
        ["fx", "remove", path, k] => project::remove_effect(Path::new(path), k, &o),
        ["mix", path] => project::mix(Path::new(path), &o),
        [] => Err(usage("no command")),
        [command, ..] => Err(usage(format!(
            "`{command}` needs other arguments, or is not a command"
        ))),
    }
}

/// A number counted from 1, as a person gives it, as an index from 0.
fn index_from_one(value: &str, what: &str) -> Result<usize> {
    match value.parse::<usize>() {
        Ok(n) if n >= 1 => Ok(n - 1),
        _ => Err(format!("{what} is counted from 1; {value} is not one").into()),
    }
}

pub(crate) struct Options {
    device: Option<String>,
    midi: Option<String>,
    pub(crate) id: Option<String>,
    plugin: Option<String>,
    save: Option<String>,
    /// Counted from 0.
    pub(crate) track: Option<usize>,
    fx: Option<usize>,
    pub(crate) master: bool,
    pub(crate) name: Option<String>,
    pub(crate) volume: Option<f32>,
    pub(crate) pan: Option<f32>,
    pub(crate) mute: Option<bool>,
    pub(crate) solo: Option<bool>,
    demo: bool,
    pub(crate) play: bool,
    no_editor: bool,
    pub(crate) tempo: Option<f64>,
    click: bool,
    no_click: bool,
    seconds: Option<f64>,
    freq: f64,
    amp: f32,
    rate: u32,
    channels: u16,
    bits: u16,
    dither: bool,
}

impl Options {
    fn parse(args: &[String]) -> Result<Self> {
        let mut o = Options {
            device: None,
            midi: None,
            id: None,
            plugin: None,
            save: None,
            track: None,
            fx: None,
            master: false,
            name: None,
            volume: None,
            pan: None,
            mute: None,
            solo: None,
            demo: false,
            play: false,
            no_editor: false,
            tempo: None,
            click: false,
            no_click: false,
            seconds: None,
            freq: 440.0,
            amp: 0.2,
            rate: 48_000,
            channels: 2,
            bits: 32,
            dither: false,
        };
        let mut it = args.iter();
        while let Some(flag) = it.next() {
            if flag == "--demo" {
                o.demo = true;
                continue;
            }
            if flag == "--play" {
                o.play = true;
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
            if flag == "--dither" {
                o.dither = true;
                continue;
            }
            let switch = match flag.as_str() {
                "--master" => Some(&mut o.master),
                _ => None,
            };
            if let Some(switch) = switch {
                *switch = true;
                continue;
            }
            let toggle = match flag.as_str() {
                "--mute" => Some((&mut o.mute, true)),
                "--unmute" => Some((&mut o.mute, false)),
                "--solo" => Some((&mut o.solo, true)),
                "--unsolo" => Some((&mut o.solo, false)),
                _ => None,
            };
            if let Some((setting, value)) = toggle {
                *setting = Some(value);
                continue;
            }
            let value = it.next().ok_or_else(|| format!("{flag} needs a value"))?;
            match flag.as_str() {
                "--device" => o.device = Some(value.clone()),
                "--midi" => o.midi = Some(value.clone()),
                "--id" => o.id = Some(value.clone()),
                "--plugin" => o.plugin = Some(value.clone()),
                "--save" => o.save = Some(value.clone()),
                "--track" => o.track = Some(index_from_one(value, "--track")?),
                "--fx" => o.fx = Some(index_from_one(value, "--fx")?),
                "--name" => o.name = Some(value.clone()),
                "--volume" => o.volume = Some(value.parse()?),
                "--pan" => o.pan = Some(value.parse()?),
                "--tempo" => o.tempo = Some(value.parse()?),
                "--seconds" => o.seconds = Some(value.parse()?),
                "--freq" => o.freq = value.parse()?,
                "--amp" => o.amp = value.parse()?,
                "--rate" => o.rate = value.parse()?,
                "--channels" => o.channels = value.parse()?,
                "--bits" => o.bits = value.parse()?,
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
        if o.master && o.track.is_some() {
            return Err("choose a track or the master, not both".into());
        }
        Ok(o)
    }

    /// The track MIDI and recording go to, counted from 0.
    fn live_track(&self) -> usize {
        self.track.unwrap_or(0)
    }

    /// The plugin whose editor opens: an effect with --fx, of the master with
    /// --master, or else the track's instrument.
    fn editor_slot(&self) -> Result<Slot> {
        match (self.fx, self.master) {
            (Some(k), true) => Ok(Slot::Master(k)),
            (Some(k), false) => Ok(Slot::Effect(self.live_track(), k)),
            (None, false) => Ok(Slot::Instrument(self.live_track())),
            (None, true) => Err("--master opens the editor of a master effect; add --fx".into()),
        }
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
    let mut session = unsafe { Session::open(Path::new(path), o.id.as_deref()) }?;
    if let Some(bpm) = o.tempo {
        session.set_tempo(bpm);
    }
    let live = o.live_track();
    session.check_track(live)?;
    let slot = o.editor_slot()?;
    let plugin_title = session.plugin(slot)?.describe();
    let tempo = session.tempo();
    let output = Output::open(o.device.as_deref())?;
    let rate = output.sample_rate();
    let mut mixer = session.mixer(rate, refrain_engine::MAX_BLOCK_FRAMES, live, &[])?;
    // The click goes on top of the mix, so no track's effects change it.
    let click = if recording { !o.no_click } else { o.click };
    if click {
        mixer = mixer.with_click(tempo, 4);
    }
    println!(
        "MIDI goes to track {}: {}",
        live + 1,
        session.plugin(Slot::Instrument(live))?.describe()
    );
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

    let stream = output.start(mixer, Some(from_notes))?;
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
    let mut tick = |session: &mut Session| {
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
        session.poll_main_thread();
        true
    };

    let shown = !o.no_editor && editor::run(&mut session, slot, &plugin_title, &mut tick)?;
    if !o.no_editor && !shown {
        println!("the plugin has no editor to show");
    }
    if !shown {
        while tick(&mut session) {
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
            println!(
                "recorded {} events into a new clip on track {}",
                events.len(),
                live + 1
            );
            session.add_clip(live, 0, &events);
        }
    }
    if let Some(project) = &save {
        session.save(Path::new(project))?;
        println!("saved {project}");
    }

    // The stream owns the plugins' processors. It has to stop before the
    // plugins are destroyed.
    drop(stream);
    drop(session);
    Ok(())
}

fn editor_only(path: &str, o: &Options) -> Result<()> {
    // SAFETY: the user asked for this file to be loaded.
    let mut session = unsafe { Session::open(Path::new(path), o.id.as_deref()) }?;
    let slot = o.editor_slot()?;
    let title = session.plugin(slot)?.describe();
    println!("{title}");
    let start = Instant::now();
    let mut tick = |session: &mut Session| {
        session.poll_main_thread();
        o.seconds.is_none_or(|s| start.elapsed().as_secs_f64() < s)
    };
    if !editor::run(&mut session, slot, &title, &mut tick)? {
        return Err("the plugin has no editor to show".into());
    }
    if let Some(project) = &o.save {
        session.save(Path::new(project))?;
        println!("saved {project}");
    }
    Ok(())
}

/// How long to wait for the sound to die away after the end, and how quiet
/// is quiet: 100 dB down, below the smallest step of a 16-bit file.
const TAIL: Tail = Tail {
    threshold: 1e-5,
    quiet_frames: 0,
    max_frames: 0,
};
const TAIL_QUIET_SECONDS: f64 = 2.0;
const TAIL_MAX_SECONDS: f64 = 60.0;

fn render(path: &str, o: &Options) -> Result<()> {
    let format = wav::Format::new(o.bits, o.dither)?;
    let rate = o.rate as f32;
    let channels = usize::from(o.channels);
    let block = 128;
    let seconds = |s: f64| (s * f64::from(o.rate)).round() as usize;

    let samples = match &o.plugin {
        None => render_offline(
            &mut Sine::new(o.freq, o.amp),
            rate,
            channels,
            seconds(o.seconds.unwrap_or(2.0)),
            block,
            &[],
        ),
        Some(plugin_path) => {
            // SAFETY: the user asked for this file to be loaded.
            let mut session = unsafe { Session::open(Path::new(plugin_path), o.id.as_deref()) }?;
            if let Some(bpm) = o.tempo {
                session.set_tempo(bpm);
            }
            let live = o.live_track();
            // The project's clips, or else the demo phrase on the track.
            let mut demo = Vec::new();
            if session.project.end() == 0 {
                session.check_track(live)?;
                println!(
                    "playing the demo phrase on track {}: {}",
                    live + 1,
                    session.plugin(Slot::Instrument(live))?.describe()
                );
                demo = demo_phrase()
                    .into_iter()
                    .map(|(t, event)| (seconds(t) as u64, event))
                    .collect::<Vec<_>>();
            }
            // To the end of the last event, then until the sound dies away;
            // or as long as --seconds says.
            let end = demo.last().map_or(session.sequence_end(rate), |e| e.0 + 1) as usize;
            let (frames, tail) = match o.seconds {
                Some(s) => (
                    seconds(s),
                    Tail {
                        threshold: 0.0,
                        ..TAIL
                    },
                ),
                None => (
                    end,
                    Tail {
                        quiet_frames: seconds(TAIL_QUIET_SECONDS),
                        max_frames: seconds(TAIL_MAX_SECONDS),
                        ..TAIL
                    },
                ),
            };
            let (took, told) = session.set_offline(true);
            println!("{took} of {told} plugins render offline");
            let mut mixer = session.mixer(rate, block, live, &demo)?;
            let render = render_until_quiet(&mut mixer, rate, channels, frames, block, &[], tail);
            if mixer.has_failed() {
                return Err("a plugin failed while processing".into());
            }
            drop(mixer);
            if !render.died_away {
                println!(
                    "the sound had not died away {TAIL_MAX_SECONDS} s after the end; cut there"
                );
            }
            render.samples
        }
    };

    let peak_of =
        |samples: &mut dyn Iterator<Item = &f32>| samples.fold(0.0_f32, |m, s| m.max(s.abs()));
    let peak = peak_of(&mut samples.iter());
    let by_channel: Vec<String> = (0..channels)
        .map(|c| {
            let peak = peak_of(&mut samples.iter().skip(c).step_by(channels));
            format!("{peak:.3}")
        })
        .collect();
    let written = samples.len() / channels;
    wav::write(Path::new(path), &samples, o.channels, o.rate, format)?;
    println!(
        "wrote {written} frames ({:.3} s, {}) to {path}, peak {peak:.3} (channels: {})",
        written as f64 / f64::from(o.rate),
        format.describe(),
        by_channel.join(" ")
    );
    let over = wav::over_full_scale(&samples);
    if over > 0 && format != wav::Format::Float32 {
        println!("{over} samples went beyond full scale and were clipped");
    }
    Ok(())
}
