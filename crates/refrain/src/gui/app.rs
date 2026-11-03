//! The main window's event loop: it draws the [`view`](super::view), carries
//! out what the person does, and keeps the audio stream and the plugins'
//! editor windows going.

use std::num::NonZeroU32;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use egui::ViewportId;
use egui_wgpu::winit::Painter;
use egui_wgpu::{RendererOptions, WgpuConfiguration};
use refrain_engine::{Event, MAX_BLOCK_FRAMES, Recording, Sequence, TICKS_PER_BEAT, Tempo};
use refrain_project::{Clip, Track};
use winit::application::ApplicationHandler;
use winit::dpi::LogicalSize;
use winit::event::WindowEvent;
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::window::{Window, WindowId};

use super::piano::PianoView;
use super::remote::{Command, Remote, Status};
use super::view::{self, Action, ChooserView, FileView, Model, Purpose, TrackView};
use crate::audio::Output;
use crate::editor::EditorWindow;
use crate::plugin::{self, Choice, Kind};
use crate::session::{Session, Slot};
use crate::{Options, Result, midi};

/// Opens the main window on the project, or plugin, at `path`, or on a new
/// project, and runs until it closes.
pub fn run(path: Option<&Path>, o: &Options) -> Result<()> {
    let session = match path {
        // SAFETY: the user asked for this file to be loaded.
        Some(path) => unsafe { Session::open(path, o.id.as_deref()) }?,
        None => Session::empty(),
    };
    let is_project = path.is_some_and(|p| {
        p.extension()
            .is_some_and(|e| e.eq_ignore_ascii_case(refrain_project::EXTENSION))
    });
    let save_to = match (&o.save, path) {
        (Some(save), _) => Some(PathBuf::from(save)),
        (None, Some(path)) if is_project => Some(path.to_path_buf()),
        _ => None,
    };
    let (output, message) = match Output::open(o.device.as_deref()) {
        Ok(output) => {
            let text = format!("output: {}", output.describe());
            (Some(output), text)
        }
        Err(e) => (None, format!("no audio output, so nothing plays: {e}")),
    };
    println!("{message}");
    let mut gui = Gui::new(session, save_to, output, message);
    gui.midi = o.midi.clone();
    gui.live = o.live_track();
    gui.stop_after = o.seconds.map(Duration::from_secs_f64);
    if !gui.session.project.tracks.is_empty() {
        gui.session.check_track(gui.live)?;
    }
    gui.start_audio(0);
    if o.play {
        gui.act(Action::Play);
    }
    let event_loop = EventLoop::new()?;
    event_loop.run_app(&mut gui)?;
    gui.shut_down();
    match gui.error {
        Some(e) => Err(e),
        None => Ok(()),
    }
}

/// The audio stream and what talks to it.
struct Audio {
    // Field order is drop order: the stream stops before MIDI is let go.
    _stream: cpal::Stream,
    commands: rtrb::Producer<Command>,
    // Sequences the audio thread is done with, to be freed here.
    spent: rtrb::Consumer<Sequence>,
    _midi: Option<midi::Connection>,
}

/// Every track's clips, as they were before an edit.
type Snapshot = Vec<Vec<Clip>>;

const TICKS_PER_BAR: u64 = 4 * TICKS_PER_BEAT;

/// A recording in progress.
struct Take {
    since: Instant,
    /// Seconds on the timeline where it began.
    from: f64,
    events: Recording,
}

struct Gui {
    title: String,
    session: Session,
    save_to: Option<PathBuf>,
    output: Option<Output>,
    audio: Option<Audio>,
    status: Arc<Status>,
    midi: Option<String>,
    // What comes in from MIDI, with the moment it came.
    recorded: Option<rtrb::Consumer<(Instant, Event)>>,
    live: usize,
    click: bool,
    dirty: bool,
    message: String,
    take: Option<Take>,
    editors: Vec<EditorWindow>,
    to_open: Vec<Slot>,
    window: Option<MainWindow>,
    error: Option<Box<dyn std::error::Error>>,
    stop_after: Option<Duration>,
    started: Instant,
    chooser: Option<Chooser>,
    // The plugin files where plugins are installed, found once.
    installed: Option<Vec<PathBuf>>,
    // The action that would throw away unsaved changes, pressed once.
    confirm: Option<&'static str>,
    // How plugins are found and loaded; tests load them in-process.
    list_file: ListFile,
    open_plugin: OpenPlugin,
    selected: Option<(usize, usize)>,
    piano: Option<(usize, usize)>,
    undo: Vec<Snapshot>,
    redo: Vec<Snapshot>,
}

type ListFile = unsafe fn(&Path) -> Result<Vec<Choice>>;
type OpenPlugin = unsafe fn(&Path, Option<&str>) -> Result<Kind>;

/// The plugin chooser, while it is open.
struct Chooser {
    purpose: Purpose,
    filter: String,
    // Files, and whether the project already uses them.
    files: Vec<(PathBuf, bool)>,
    file: Option<usize>,
    picked: Option<PathBuf>,
    plugins: Vec<Choice>,
    path: String,
}

impl Gui {
    fn new(
        session: Session,
        save_to: Option<PathBuf>,
        output: Option<Output>,
        message: String,
    ) -> Self {
        let mut gui = Self {
            title: String::new(),
            session,
            save_to,
            output,
            audio: None,
            status: Arc::new(Status::default()),
            midi: None,
            recorded: None,
            live: 0,
            click: true,
            dirty: false,
            message,
            take: None,
            editors: Vec::new(),
            to_open: Vec::new(),
            window: None,
            error: None,
            stop_after: None,
            started: Instant::now(),
            chooser: None,
            installed: None,
            confirm: None,
            list_file: plugin::list_file,
            open_plugin: Kind::open,
            selected: None,
            piano: None,
            undo: Vec::new(),
            redo: Vec::new(),
        };
        gui.retitle();
        gui
    }

    fn rate(&self) -> f32 {
        self.output.as_ref().map_or(48_000.0, Output::sample_rate)
    }

    /// Activates the plugins and starts the audio stream, stopped, with the
    /// playhead at `frame`. Without an output, or on an error, nothing plays
    /// and the message says why.
    fn start_audio(&mut self, frame: u64) {
        if let Err(e) = self.try_start_audio(frame) {
            self.message = format!("cannot play: {e}");
        }
    }

    fn try_start_audio(&mut self, frame: u64) -> Result<()> {
        let Some(output) = &self.output else {
            return Ok(());
        };
        let rate = output.sample_rate();
        let tempo = self.session.tempo();
        let mut mixer = self
            .session
            .mixer(rate, MAX_BLOCK_FRAMES, self.live, &[])?
            .with_click(tempo, 4)
            .stopped();
        mixer.set_click(self.click);
        let (mut commands, from_gui) = rtrb::RingBuffer::new(256);
        let _ = commands.push(Command::Locate(frame));
        let (notes, from_midi) = rtrb::RingBuffer::new(1024);
        let (record, recorded) = rtrb::RingBuffer::new(4096);
        let midi = match midi::connect(self.midi.as_deref(), notes, Some(record)) {
            Ok(midi) => midi,
            Err(e) => {
                self.message = format!("no MIDI input: {e}");
                None
            }
        };
        self.status = Arc::new(Status::default());
        let (to_free, spent) = rtrb::RingBuffer::new(64);
        let remote = Remote::new(mixer, from_gui, to_free, Arc::clone(&self.status));
        let stream = output.start(remote, Some(from_midi))?;
        self.recorded = Some(recorded);
        self.audio = Some(Audio {
            _stream: stream,
            commands,
            spent,
            _midi: midi,
        });
        Ok(())
    }

    /// Stops the stream and lets the plugins go, and returns where the
    /// playhead was.
    fn stop_audio(&mut self) -> u64 {
        let frame = self.status.playhead();
        self.audio = None;
        self.recorded = None;
        frame
    }

    /// Puts the mixer together again, after the tracks or the tempo changed.
    fn rebuild(&mut self) {
        let frame = self.stop_audio();
        self.start_audio(frame);
    }

    fn send(&mut self, command: Command) {
        if let Some(audio) = &mut self.audio {
            let _ = audio.commands.push(command);
        }
    }

    fn seconds(&self, frame: u64) -> f64 {
        frame as f64 / f64::from(self.rate())
    }

    fn frame(&self, seconds: f64) -> u64 {
        (seconds.max(0.0) * f64::from(self.rate())).round() as u64
    }

    fn model(&self) -> Model {
        let project = &self.session.project;
        let tempo = project.tempo();
        Model {
            title: self.title.clone(),
            tracks: project
                .tracks
                .iter()
                .map(|t| {
                    let effects = t.effects.iter().map(|e| e.name.clone()).collect();
                    TrackView::new(t, tempo, t.instrument.name.clone(), effects)
                })
                .collect(),
            master_volume_db: project.master.volume_db,
            master_effects: project
                .master
                .effects
                .iter()
                .map(|e| e.name.clone())
                .collect(),
            live: self.live,
            playing: self.status.playing(),
            recording: self.take.is_some(),
            can_play: self.audio.is_some(),
            playhead: self.seconds(self.status.playhead()),
            tempo: project.tempo,
            click: self.click,
            dirty: self.dirty,
            status: if self.status.failed() {
                "a plugin failed and gives silence".into()
            } else {
                self.message.clone()
            },
            chooser: self.chooser.as_ref().map(|c| ChooserView {
                purpose: Some(c.purpose),
                filter: c.filter.clone(),
                files: c
                    .files
                    .iter()
                    .map(|(path, in_project)| FileView {
                        path: path.display().to_string(),
                        in_project: *in_project,
                    })
                    .collect(),
                file: c.file,
                plugins: c.plugins.clone(),
                path: c.path.clone(),
            }),
            selected: self.selected,
            piano: self.piano.and_then(|(t, c)| {
                let clip = project.tracks.get(t)?.clips.get(c)?;
                let (notes, _) = clip.notes();
                let end = notes
                    .iter()
                    .map(|n| n.start + n.length)
                    .fold(clip.length(), u64::max);
                // The clip and a bar or so after it, in whole bars.
                let length = (end + 2 * TICKS_PER_BEAT).div_ceil(TICKS_PER_BAR) * TICKS_PER_BAR;
                Some(PianoView {
                    track: t,
                    clip: c,
                    notes,
                    length: length.max(4 * TICKS_PER_BAR),
                })
            }),
            can_undo: !self.undo.is_empty(),
            can_redo: !self.redo.is_empty(),
        }
    }

    fn snapshot(&self) -> Snapshot {
        self.session
            .project
            .tracks
            .iter()
            .map(|t| t.clips.clone())
            .collect()
    }

    /// Plays track `t`'s clips as they are now, from the next block.
    fn resend(&mut self, t: usize) {
        let sequence = self.session.sequence(t, self.rate());
        self.send(Command::Sequence(t, sequence));
    }

    /// Changes track `t`'s clips with `f`, which says whether it changed
    /// anything. The change can be undone, and is heard from the next block.
    fn edit_clips(&mut self, t: usize, f: impl FnOnce(&mut Track) -> bool) -> bool {
        let before = self.snapshot();
        let Some(track) = self.session.project.tracks.get_mut(t) else {
            return false;
        };
        if !f(track) {
            return false;
        }
        self.undo.push(before);
        self.redo.clear();
        self.dirty = true;
        self.resend(t);
        true
    }

    /// Puts every track's clips back as `snapshot` has them, and returns how
    /// they were.
    fn restore(&mut self, snapshot: Snapshot) -> Snapshot {
        let now = self.snapshot();
        for (track, clips) in self.session.project.tracks.iter_mut().zip(snapshot) {
            track.clips = clips;
        }
        for t in 0..self.session.project.tracks.len() {
            self.resend(t);
        }
        self.dirty = true;
        // A clip that is gone can be neither picked nor edited.
        let exists = |p: &Option<(usize, usize)>, s: &Session| {
            p.is_some_and(|(t, c)| s.project.tracks.get(t).is_some_and(|t| c < t.clips.len()))
        };
        if !exists(&self.selected, &self.session) {
            self.selected = None;
        }
        if !exists(&self.piano, &self.session) {
            self.piano = None;
        }
        now
    }

    /// Forgets what could be undone, after the tracks themselves changed.
    fn forget_edits(&mut self) {
        self.undo.clear();
        self.redo.clear();
        self.selected = None;
        self.piano = None;
    }

    fn act(&mut self, action: Action) {
        if !matches!(action, Action::New | Action::Open) {
            self.confirm = None;
        }
        match action {
            Action::Play => self.send(Command::Play),
            Action::Stop => {
                self.send(Command::Stop);
                if self.take.is_some() {
                    self.finish_take();
                }
            }
            Action::Record => {
                // What came in before the button was pressed is not part of it.
                if let Some(recorded) = &mut self.recorded {
                    while recorded.pop().is_ok() {}
                }
                self.take = Some(Take {
                    since: Instant::now(),
                    from: self.seconds(self.status.playhead()),
                    events: Recording::default(),
                });
                self.send(Command::Play);
                self.message = format!("recording on track {}", self.live + 1);
            }
            Action::ToStart => self.send(Command::Locate(0)),
            Action::Locate(seconds) => {
                let frame = self.frame(seconds);
                self.send(Command::Locate(frame));
            }
            Action::Click(on) => {
                self.click = on;
                self.send(Command::Click(on));
            }
            Action::Tempo(bpm) => {
                self.session.set_tempo(bpm);
                self.dirty = true;
                self.rebuild();
            }
            Action::LiveTrack(track) => {
                self.live = track;
                self.send(Command::LiveTrack(track));
            }
            Action::Mix(track, mix) => {
                if let Some(t) = self.session.project.tracks.get_mut(track) {
                    t.mix = mix;
                    self.dirty = true;
                    self.send(Command::Mix(track, mix.to_engine()));
                }
            }
            Action::MasterVolume(db) => {
                self.session.project.master.volume_db = db;
                self.dirty = true;
                self.send(Command::MasterVolume(db));
            }
            Action::OpenEditor(slot) => {
                if !self.editors.iter().any(|e| e.slot == slot) {
                    self.to_open.push(slot);
                }
            }
            Action::Save if self.save_to.is_none() => self.save_as(),
            Action::Save => self.save(),
            Action::SaveAs => self.save_as(),
            Action::New => {
                if self.may_discard("New") {
                    self.replace(Session::empty(), None);
                }
            }
            Action::Open => {
                if self.may_discard("Open")
                    && let Some(path) = rfd::FileDialog::new()
                        .add_filter("refrain project", &[refrain_project::EXTENSION])
                        .pick_file()
                {
                    self.open_project(&path);
                }
            }
            Action::Choose(purpose) => self.open_chooser(purpose),
            Action::Filter(filter) => {
                if let Some(c) = &mut self.chooser {
                    c.filter = filter;
                }
            }
            Action::TypePath(path) => {
                if let Some(c) = &mut self.chooser {
                    c.path = path;
                }
            }
            Action::PickFile(i) => {
                let Some(c) = &mut self.chooser else { return };
                let Some((path, _)) = c.files.get(i) else {
                    return;
                };
                let path = path.clone();
                c.file = Some(i);
                self.pick_path(path);
            }
            Action::PickPath(typed) => {
                let path = match typed {
                    Some(typed) => Some(PathBuf::from(typed)),
                    None => rfd::FileDialog::new()
                        .add_filter("CLAP or VST3 plugin", &["clap", "vst3"])
                        .pick_file(),
                };
                if let Some(path) = path {
                    if let Some(c) = &mut self.chooser {
                        c.file = None;
                    }
                    self.pick_path(path);
                }
            }
            Action::PickPlugin(i) => {
                let Some(c) = &self.chooser else { return };
                let (Some(path), Some(choice)) = (c.picked.clone(), c.plugins.get(i)) else {
                    return;
                };
                let id = choice.id.clone();
                self.add_plugin(&path, &id);
            }
            Action::CloseChooser => self.chooser = None,
            Action::RemoveTrack(t) => {
                let name = self.session.project.tracks.get(t).map(|t| t.name.clone());
                if self.change(|s| s.remove_track(t)) {
                    if self.live > t || self.live >= self.session.project.tracks.len() {
                        self.live = self.live.saturating_sub(1);
                    }
                    self.message = format!("removed track {}: {}", t + 1, name.unwrap_or_default());
                }
            }
            Action::RemoveEffect(chain, k) => {
                if self.change(|s| s.remove_effect(chain, k)) {
                    self.message = "removed the effect".into();
                }
            }
            Action::SelectClip(selected) => self.selected = selected,
            Action::MoveClip(t, c, start) => {
                if self.edit_clips(t, |track| {
                    let moved = track.clips.get(c).is_some_and(|clip| clip.start != start);
                    track.move_clip(c, start);
                    moved
                }) {
                    self.selected = Some((t, c));
                }
            }
            Action::DuplicateClip(t, c) => {
                let mut copy = None;
                self.edit_clips(t, |track| {
                    copy = track.duplicate_clip(c, TICKS_PER_BAR);
                    copy.is_some()
                });
                if let Some(copy) = copy {
                    self.selected = Some((t, copy));
                }
            }
            Action::DeleteClip(t, c) => {
                if self.edit_clips(t, |track| track.remove_clip(c).is_some()) {
                    // Clips after it on the track move up.
                    self.selected = None;
                    if self.piano.is_some_and(|(pt, _)| pt == t) {
                        self.piano = None;
                    }
                }
            }
            Action::OpenPiano(t, c) => {
                let exists = self
                    .session
                    .project
                    .tracks
                    .get(t)
                    .is_some_and(|track| c < track.clips.len());
                if exists {
                    self.piano = Some((t, c));
                    self.selected = Some((t, c));
                }
            }
            Action::ClosePiano => self.piano = None,
            Action::SetNotes(t, c, notes) => {
                self.edit_clips(t, |track| {
                    let Some(clip) = track.clips.get(c) else {
                        return false;
                    };
                    let (old, others) = clip.notes();
                    if old == notes {
                        return false;
                    }
                    track.clips[c] = clip.with_notes(&notes, &others);
                    true
                });
            }
            Action::Undo => {
                if let Some(snapshot) = self.undo.pop() {
                    let now = self.restore(snapshot);
                    self.redo.push(now);
                }
            }
            Action::Redo => {
                if let Some(snapshot) = self.redo.pop() {
                    let now = self.restore(snapshot);
                    self.undo.push(now);
                }
            }
        }
    }

    /// Whether to go on with `what`, which throws away unsaved changes: yes
    /// when there are none, or when it is pressed a second time.
    fn may_discard(&mut self, what: &'static str) -> bool {
        if !self.dirty || self.confirm == Some(what) {
            self.confirm = None;
            return true;
        }
        self.confirm = Some(what);
        self.message = format!("there are unsaved changes; press {what} again to throw them away");
        false
    }

    /// Changes the tracks or the effects: closes the editors, whose slots
    /// may move, and puts the mixer together again around the change.
    /// Returns whether it went through.
    fn change(&mut self, f: impl FnOnce(&mut Session) -> Result<()>) -> bool {
        while !self.editors.is_empty() {
            self.close_editor(0);
        }
        self.forget_edits();
        let frame = self.stop_audio();
        let result = f(&mut self.session);
        if let Err(e) = &result {
            self.message = e.to_string();
        } else {
            self.dirty = true;
        }
        self.start_audio(frame);
        result.is_ok()
    }

    fn open_chooser(&mut self, purpose: Purpose) {
        let installed = self.installed.get_or_insert_with(plugin::installed_files);
        let in_project = self.session.plugin_files();
        let mut files: Vec<(PathBuf, bool)> =
            in_project.iter().map(|f| (f.clone(), true)).collect();
        files.extend(
            installed
                .iter()
                .filter(|f| !in_project.contains(f))
                .map(|f| (f.clone(), false)),
        );
        self.chooser = Some(Chooser {
            purpose,
            filter: String::new(),
            files,
            file: None,
            picked: None,
            plugins: Vec::new(),
            path: String::new(),
        });
    }

    /// Lists the plugins in `path`. When it holds just one, that one is
    /// added straight away.
    fn pick_path(&mut self, path: PathBuf) {
        // SAFETY: the person picked this file to load.
        let mut plugins = match unsafe { (self.list_file)(&path) } {
            Ok(plugins) if !plugins.is_empty() => plugins,
            Ok(_) => {
                self.message = format!("{} holds no plugins", path.display());
                return;
            }
            Err(e) => {
                self.message = format!("{}: {e}", path.display());
                return;
            }
        };
        let Some(c) = &mut self.chooser else { return };
        // What the chooser is for comes first.
        let want_effect = matches!(c.purpose, Purpose::Effect(_));
        plugins.sort_by_key(|p| p.effect != want_effect);
        if let [only] = plugins.as_slice() {
            let id = only.id.clone();
            self.add_plugin(&path, &id);
            return;
        }
        c.picked = Some(path);
        c.plugins = plugins;
    }

    /// Loads plugin `id` from `path` and adds it where the chooser said.
    fn add_plugin(&mut self, path: &Path, id: &str) {
        let Some(purpose) = self.chooser.as_ref().map(|c| c.purpose) else {
            return;
        };
        // SAFETY: the person picked this plugin to load.
        let kind = match unsafe { (self.open_plugin)(path, Some(id)) } {
            Ok(kind) => kind,
            Err(e) => {
                self.message = format!("could not load {}: {e}", path.display());
                return;
            }
        };
        let name = kind.describe();
        let added = self.change(|s| match purpose {
            Purpose::Track => {
                s.add_track(path, kind);
                Ok(())
            }
            Purpose::Effect(chain) => s.add_effect(chain, path, kind),
        });
        if added {
            if purpose == Purpose::Track {
                // A new track is usually the one to play next.
                self.live = self.session.project.tracks.len() - 1;
                self.send(Command::LiveTrack(self.live));
            }
            self.chooser = None;
            self.message = format!("added {name}");
        }
    }

    fn open_project(&mut self, path: &Path) {
        // SAFETY: the person picked this project to open.
        match unsafe { Session::open(path, None) } {
            Ok(session) => self.replace(session, Some(path.to_path_buf())),
            Err(e) => self.message = format!("could not open {}: {e}", path.display()),
        }
    }

    /// Puts another session in place of this one, with everything that
    /// belongs to the old one let go first.
    fn replace(&mut self, session: Session, save_to: Option<PathBuf>) {
        while !self.editors.is_empty() {
            self.close_editor(0);
        }
        self.stop_audio();
        self.take = None;
        self.chooser = None;
        self.forget_edits();
        self.session = session;
        self.save_to = save_to;
        self.live = 0;
        self.dirty = false;
        self.retitle();
        self.message = match &self.save_to {
            Some(path) => format!("opened {}", path.display()),
            None => "a new project".into(),
        };
        self.start_audio(0);
    }

    fn retitle(&mut self) {
        let name = self
            .save_to
            .as_ref()
            .map_or_else(|| "untitled".into(), |p| p.display().to_string());
        self.title = format!("refrain — {name}");
        if let Some(window) = &self.window {
            window.window.set_title(&self.title);
        }
    }

    fn save_as(&mut self) {
        let Some(mut path) = rfd::FileDialog::new()
            .add_filter("refrain project", &[refrain_project::EXTENSION])
            .set_file_name("untitled.refrain")
            .save_file()
        else {
            return;
        };
        if path.extension().is_none() {
            path.set_extension(refrain_project::EXTENSION);
        }
        self.save_to = Some(path);
        self.save();
        self.retitle();
    }

    /// Collects what came in from MIDI while recording; drops it otherwise.
    fn collect_midi(&mut self) {
        let Some(recorded) = &mut self.recorded else {
            return;
        };
        while let Ok((at, event)) = recorded.pop() {
            if let Some(take) = &mut self.take {
                let seconds = at.saturating_duration_since(take.since).as_secs_f64();
                take.events.add(seconds, event);
            }
        }
    }

    /// Ends the recording: what was played becomes a clip where the
    /// recording began, and the mixer is put together again to play it.
    fn finish_take(&mut self) {
        self.collect_midi();
        let Some(take) = self.take.take() else {
            return;
        };
        if take.events.is_empty() {
            self.message = "nothing was played, so there is no new clip".into();
            return;
        }
        let tempo = self.session.tempo();
        let played = take.events.finish(take.since.elapsed().as_secs_f64());
        let (start, events) = clip_from(&played, take.from, tempo);
        let count = events.len();
        let live = self.live;
        self.edit_clips(live, |track| {
            track.add_clip(start, &events);
            true
        });
        self.message = format!(
            "recorded {count} events into a new clip on track {}",
            live + 1
        );
    }

    fn save(&mut self) {
        let Some(file) = self.save_to.clone() else {
            return;
        };
        self.message = match self.session.save(&file) {
            Ok(()) => {
                self.dirty = false;
                format!("saved {}", file.display())
            }
            Err(e) => format!("could not save: {e}"),
        };
        println!("{}", self.message);
    }

    fn open_editors(&mut self, event_loop: &ActiveEventLoop) {
        for slot in std::mem::take(&mut self.to_open) {
            let title = match self.session.plugin(slot) {
                Ok(plugin) => plugin.describe(),
                Err(e) => {
                    self.message = e.to_string();
                    continue;
                }
            };
            match EditorWindow::open(event_loop, &mut self.session, slot, &title) {
                Ok(Some(editor)) => self.editors.push(editor),
                Ok(None) => self.message = format!("{title} has no editor to show"),
                Err(e) => self.message = format!("could not open the editor: {e}"),
            }
        }
    }

    fn close_editor(&mut self, index: usize) {
        let editor = self.editors.remove(index);
        editor.close(&mut self.session);
    }

    /// Closes everything, in order: editors before their plugins, the audio
    /// stream before the plugins it runs.
    fn shut_down(&mut self) {
        while !self.editors.is_empty() {
            self.close_editor(0);
        }
        let frame = self.stop_audio();
        if self.output.is_some() {
            println!("the playhead was at {:.2} s", self.seconds(frame));
        }
        self.window = None;
        if self.dirty {
            println!("closed with unsaved changes");
        }
    }
}

/// A clip from what was recorded starting at `from` seconds on the timeline:
/// the tick it starts at, and its events at ticks from there. `played` has
/// seconds from the start of the recording.
fn clip_from(played: &[(f64, Event)], from: f64, tempo: Tempo) -> (u64, Vec<(u64, Event)>) {
    let start = tempo.ticks_at(from);
    let events = played
        .iter()
        .map(|&(seconds, event)| (tempo.ticks_at(from + seconds) - start, event))
        .collect();
    (start, events)
}

impl ApplicationHandler for Gui {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.window.is_some() {
            return;
        }
        match MainWindow::new(event_loop, &self.title) {
            Ok(window) => self.window = Some(window),
            Err(e) => {
                self.error = Some(e);
                event_loop.exit();
            }
        }
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, id: WindowId, event: WindowEvent) {
        if let Some(i) = self.editors.iter().position(|e| e.id() == id) {
            if !self.editors[i].on_event(&mut self.session, &event) {
                self.close_editor(i);
            }
            return;
        }
        if self.window.is_none() {
            return;
        }
        match event {
            WindowEvent::CloseRequested => event_loop.exit(),
            WindowEvent::RedrawRequested => {
                let model = self.model();
                let actions = self.window.as_mut().map(|w| w.paint(&model));
                for action in actions.into_iter().flatten() {
                    self.act(action);
                }
            }
            event => {
                if let Some(window) = &mut self.window {
                    window.on_event(&event);
                }
            }
        }
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        if self
            .stop_after
            .is_some_and(|after| self.started.elapsed() >= after)
        {
            event_loop.exit();
            return;
        }
        self.session.poll_main_thread();
        self.collect_midi();
        if let Some(audio) = &mut self.audio {
            while audio.spent.pop().is_ok() {}
        }
        let mut i = 0;
        while i < self.editors.len() {
            if self.editors[i].poll(&mut self.session) {
                i += 1;
            } else {
                self.close_editor(i);
            }
        }
        self.open_editors(event_loop);
        if let Some(window) = &self.window {
            window.window.request_redraw();
        }
        // About 30 frames a second: enough for the playhead, and often enough
        // for the plugins' main-thread requests.
        event_loop.set_control_flow(ControlFlow::WaitUntil(
            Instant::now() + Duration::from_millis(33),
        ));
    }
}

/// The main window, drawn by egui through wgpu.
struct MainWindow {
    // Field order is drop order: the painter lets go of the window's surface
    // before the window goes.
    painter: Painter,
    state: egui_winit::State,
    ctx: egui::Context,
    window: Arc<Window>,
    first_frame: bool,
}

impl MainWindow {
    fn new(event_loop: &ActiveEventLoop, title: &str) -> Result<Self> {
        let attributes = Window::default_attributes()
            .with_title(title)
            .with_inner_size(LogicalSize::new(1_200.0, 720.0))
            .with_min_inner_size(LogicalSize::new(800.0, 400.0));
        let window = Arc::new(event_loop.create_window(attributes)?);
        let ctx = egui::Context::default();
        super::fonts::install(&ctx)?;
        let mut painter = pollster::block_on(Painter::new(
            ctx.clone(),
            WgpuConfiguration::default(),
            false,
            RendererOptions::default(),
        ));
        pollster::block_on(painter.set_window(ViewportId::ROOT, Some(Arc::clone(&window))))?;
        if let Some(state) = painter.render_state() {
            let info = state.adapter.get_info();
            println!("window: drawn with {:?} on {}", info.backend, info.name);
        }
        let state = egui_winit::State::new(
            ctx.clone(),
            ViewportId::ROOT,
            &window,
            Some(window.scale_factor() as f32),
            None,
            painter.max_texture_side(),
        );
        Ok(Self {
            painter,
            state,
            ctx,
            window,
            first_frame: true,
        })
    }

    fn on_event(&mut self, event: &WindowEvent) {
        let response = self.state.on_window_event(&self.window, event);
        if let WindowEvent::Resized(size) = event
            && let (Some(w), Some(h)) = (NonZeroU32::new(size.width), NonZeroU32::new(size.height))
        {
            self.painter.on_window_resized(ViewportId::ROOT, w, h);
        }
        if response.repaint {
            self.window.request_redraw();
        }
    }

    /// Draws a frame of `model`, and returns what the person did.
    fn paint(&mut self, model: &Model) -> Vec<Action> {
        let input = self.state.take_egui_input(&self.window);
        let mut actions = Vec::new();
        let mut output = self.ctx.run_ui(input, |ui| actions = view::show(ui, model));
        self.state
            .handle_platform_output(&self.window, std::mem::take(&mut output.platform_output));
        let primitives = self.ctx.tessellate(output.shapes, output.pixels_per_point);
        self.painter.paint_and_update_textures(
            ViewportId::ROOT,
            output.pixels_per_point,
            [0.0, 0.0, 0.0, 1.0],
            &primitives,
            &mut output.textures_delta,
            Vec::new(),
            &self.window,
        );
        if self.first_frame {
            self.first_frame = false;
            println!("window: first frame drawn");
        }
        actions
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_recording_becomes_a_clip_where_it_began() {
        let on = Event::NoteOn {
            channel: 0,
            key: 60,
            velocity: 1.0,
        };
        // At 120 beats per minute a beat is half a second: 960 ticks.
        let tempo = Tempo::new(120.0);
        let (start, events) = clip_from(&[(0.0, on), (0.25, on)], 1.0, tempo);
        assert_eq!(start, 1_920);
        assert_eq!(events, [(0, on), (480, on)]);
        // The events land where they were played on the timeline.
        assert_eq!(start + events[1].0, tempo.ticks_at(1.25));
    }
}

#[cfg(test)]
mod flow_tests {
    use clack_host::prelude::PluginEntry;
    use refrain_clap::ClapPlugin;

    use super::*;
    use crate::session::Chain;

    // The CLAP test plugins, loaded in-process whatever the path says.
    unsafe fn list(path: &Path) -> Result<Vec<Choice>> {
        if path.ends_with("missing.clap") {
            return Err("no such file".into());
        }
        let entry = PluginEntry::load_from_clack::<refrain_test_synth::Entry>(c"t").unwrap();
        Ok(refrain_clap::plugins_in(&entry)?
            .into_iter()
            .map(|p| Choice {
                effect: p.is_effect(),
                id: p.id,
                name: p.name,
            })
            .collect())
    }

    unsafe fn open(_path: &Path, id: Option<&str>) -> Result<Kind> {
        let entry = PluginEntry::load_from_clack::<refrain_test_synth::Entry>(c"t").unwrap();
        Ok(Kind::Clap(ClapPlugin::new(&entry, id)?))
    }

    /// A window's worth of state on a new project, with no audio output.
    fn gui() -> Gui {
        let mut gui = Gui::new(Session::empty(), None, None, String::new());
        gui.list_file = list;
        gui.open_plugin = open;
        gui
    }

    fn pick(gui: &mut Gui, purpose: Purpose, name: &str) {
        gui.act(Action::Choose(purpose));
        gui.act(Action::PickPath(Some("plugins/synth.clap".into())));
        let chooser = gui.model().chooser.expect("the chooser is open");
        let i = chooser.plugins.iter().position(|p| p.name == name).unwrap();
        gui.act(Action::PickPlugin(i));
    }

    #[test]
    fn choosing_plugins_adds_tracks_and_effects() {
        let mut gui = gui();
        pick(&mut gui, Purpose::Track, "refrain test synth");
        assert!(gui.chooser.is_none());
        pick(&mut gui, Purpose::Track, "refrain test synth (notes only)");
        pick(
            &mut gui,
            Purpose::Effect(Chain::Track(0)),
            "refrain test effect",
        );
        pick(
            &mut gui,
            Purpose::Effect(Chain::Master),
            "refrain test effect",
        );
        let model = gui.model();
        let names: Vec<&str> = model.tracks.iter().map(|t| t.instrument.as_str()).collect();
        assert_eq!(
            names,
            ["refrain test synth", "refrain test synth (notes only)"]
        );
        assert_eq!(model.tracks[0].effects, ["refrain test effect"]);
        assert!(model.tracks[1].effects.is_empty());
        assert_eq!(model.master_effects, ["refrain test effect"]);
        // MIDI goes to the newest track; the project has changes to save.
        assert_eq!((model.live, model.dirty), (1, true));
    }

    #[test]
    fn the_chooser_lists_what_it_is_for_first_and_says_what_went_wrong() {
        let mut gui = gui();
        gui.act(Action::Choose(Purpose::Effect(Chain::Master)));
        gui.act(Action::PickPath(Some("plugins/synth.clap".into())));
        let chooser = gui.model().chooser.unwrap();
        assert!(chooser.plugins[0].effect);
        assert!(!chooser.plugins[1].effect);
        gui.act(Action::PickPath(Some("plugins/missing.clap".into())));
        assert!(
            gui.model().status.contains("no such file"),
            "{}",
            gui.model().status
        );
        // Picking a file that failed leaves the chooser open.
        assert!(gui.chooser.is_some());
        gui.act(Action::CloseChooser);
        assert!(gui.chooser.is_none());
    }

    #[test]
    fn removing_a_track_keeps_midi_on_a_track_that_is_there() {
        let mut gui = gui();
        for _ in 0..3 {
            pick(&mut gui, Purpose::Track, "refrain test synth");
        }
        assert_eq!(gui.live, 2);
        gui.act(Action::RemoveTrack(2));
        assert_eq!((gui.live, gui.model().tracks.len()), (1, 2));
        pick(&mut gui, Purpose::Track, "refrain test synth");
        gui.act(Action::LiveTrack(1));
        gui.act(Action::RemoveTrack(0));
        // The track MIDI went to moved up.
        assert_eq!((gui.live, gui.model().tracks.len()), (0, 2));
        gui.act(Action::RemoveTrack(5));
        assert!(
            gui.model().status.contains("no track 6"),
            "{}",
            gui.model().status
        );
    }

    #[test]
    fn effects_come_off_the_chain_they_are_on() {
        let mut gui = gui();
        pick(&mut gui, Purpose::Track, "refrain test synth");
        pick(
            &mut gui,
            Purpose::Effect(Chain::Track(0)),
            "refrain test effect",
        );
        pick(
            &mut gui,
            Purpose::Effect(Chain::Master),
            "refrain test effect",
        );
        gui.act(Action::RemoveEffect(Chain::Track(0), 0));
        let model = gui.model();
        assert!(model.tracks[0].effects.is_empty());
        assert_eq!(model.master_effects.len(), 1);
    }

    /// A window with one track, holding one clip of a C at its start.
    fn with_a_clip() -> Gui {
        let mut gui = gui();
        pick(&mut gui, Purpose::Track, "refrain test synth");
        let on = Event::NoteOn {
            channel: 0,
            key: 60,
            velocity: 100.0 / 127.0,
        };
        let off = Event::NoteOff {
            channel: 0,
            key: 60,
            velocity: 64.0 / 127.0,
        };
        gui.session.project.tracks[0].add_clip(0, &[(0, on), (480, off)]);
        gui.dirty = false;
        gui
    }

    fn starts(gui: &Gui) -> Vec<u64> {
        gui.session.project.tracks[0]
            .clips
            .iter()
            .map(|c| c.start)
            .collect()
    }

    #[test]
    fn clips_move_copy_and_go_and_every_edit_can_be_undone() {
        let mut gui = with_a_clip();
        assert!(!gui.model().can_undo);
        gui.act(Action::MoveClip(0, 0, 1_920));
        assert_eq!(starts(&gui), [1_920]);
        assert!(gui.model().dirty && gui.model().can_undo);
        gui.act(Action::DuplicateClip(0, 0));
        // After the clip, at the next bar.
        assert_eq!(starts(&gui), [1_920, 3_840]);
        assert_eq!(gui.model().selected, Some((0, 1)));
        gui.act(Action::DeleteClip(0, 0));
        assert_eq!(starts(&gui), [3_840]);
        assert_eq!(gui.model().selected, None);

        gui.act(Action::Undo);
        assert_eq!(starts(&gui), [1_920, 3_840]);
        gui.act(Action::Undo);
        gui.act(Action::Undo);
        assert_eq!(starts(&gui), [0]);
        assert!(!gui.model().can_undo && gui.model().can_redo);
        gui.act(Action::Redo);
        assert_eq!(starts(&gui), [1_920]);
        // A new edit forgets what could be redone.
        gui.act(Action::MoveClip(0, 0, 0));
        assert!(!gui.model().can_redo);
        // Moving a clip where it is changes nothing to undo.
        let undoable = gui.undo.len();
        gui.act(Action::MoveClip(0, 0, 0));
        assert_eq!(gui.undo.len(), undoable);
    }

    #[test]
    fn the_piano_roll_edits_notes_and_keeps_the_rest() {
        let mut gui = with_a_clip();
        // A pedal in the clip, which the piano roll does not show.
        gui.session.project.tracks[0].clips[0]
            .events
            .insert(0, (0, vec![0xb0, 64, 127]));
        gui.act(Action::OpenPiano(0, 0));
        let piano = gui.model().piano.expect("the piano roll is open");
        assert_eq!(piano.notes.len(), 1);
        assert_eq!(piano.length % TICKS_PER_BAR, 0);
        let mut notes = piano.notes.clone();
        notes[0].key = 64;
        notes.push(refrain_project::Note {
            start: 960,
            key: 67,
            ..notes[0]
        });
        gui.act(Action::SetNotes(0, 0, notes.clone()));
        assert_eq!(gui.model().piano.unwrap().notes, notes);
        let clip = &gui.session.project.tracks[0].clips[0];
        assert!(clip.events.contains(&(0, vec![0xb0, 64, 127])));
        // What plays is what the piano roll shows.
        let keys: Vec<u8> = gui.session.project.tracks[0]
            .sequence()
            .into_iter()
            .filter_map(|(_, e)| match e {
                Event::NoteOn { key, .. } => Some(key),
                _ => None,
            })
            .collect();
        assert_eq!(keys, [64, 67]);
        gui.act(Action::Undo);
        assert_eq!(gui.model().piano.unwrap().notes[0].key, 60);
        // Deleting the clip closes its piano roll.
        gui.act(Action::DeleteClip(0, 0));
        assert!(gui.model().piano.is_none());
    }

    #[test]
    fn changing_the_tracks_forgets_what_could_be_undone() {
        let mut gui = with_a_clip();
        gui.act(Action::MoveClip(0, 0, 960));
        pick(&mut gui, Purpose::Track, "refrain test synth");
        assert!(!gui.model().can_undo);
        gui.act(Action::Undo);
        assert_eq!(starts(&gui), [960]);
    }

    #[test]
    fn unsaved_changes_are_thrown_away_only_on_a_second_press() {
        let mut gui = gui();
        pick(&mut gui, Purpose::Track, "refrain test synth");
        gui.act(Action::New);
        assert_eq!(gui.model().tracks.len(), 1);
        assert!(gui.model().status.contains("press New again"));
        // Something else in between asks again.
        gui.act(Action::Click(false));
        gui.act(Action::New);
        assert_eq!(gui.model().tracks.len(), 1);
        gui.act(Action::New);
        let model = gui.model();
        assert!(model.tracks.is_empty() && !model.dirty);
        assert_eq!(model.title, "refrain — untitled");
    }
}
