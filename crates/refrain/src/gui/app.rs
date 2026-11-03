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
use refrain_engine::{Event, MAX_BLOCK_FRAMES, Recording, Tempo};
use winit::application::ApplicationHandler;
use winit::dpi::LogicalSize;
use winit::event::WindowEvent;
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::window::{Window, WindowId};

use super::remote::{Command, Remote, Status};
use super::view::{self, Action, Model, TrackView};
use crate::audio::Output;
use crate::editor::EditorWindow;
use crate::session::{Session, Slot};
use crate::{Options, Result, midi};

/// Opens the main window on the project, or plugin, at `path`, and runs until
/// it closes.
pub fn run(path: &Path, o: &Options) -> Result<()> {
    // SAFETY: the user asked for this file to be loaded.
    let session = unsafe { Session::open(path, o.id.as_deref()) }?;
    let is_project = path
        .extension()
        .is_some_and(|e| e.eq_ignore_ascii_case(refrain_project::EXTENSION));
    let save_to = match &o.save {
        Some(save) => Some(PathBuf::from(save)),
        None if is_project => Some(path.to_path_buf()),
        None => None,
    };
    let (output, message) = match Output::open(o.device.as_deref()) {
        Ok(output) => {
            let text = format!("output: {}", output.describe());
            (Some(output), text)
        }
        Err(e) => (None, format!("no audio output, so nothing plays: {e}")),
    };
    println!("{message}");
    let mut gui = Gui {
        title: format!("refrain — {}", path.display()),
        session,
        save_to,
        output,
        audio: None,
        status: Arc::new(Status::default()),
        midi: o.midi.clone(),
        recorded: None,
        live: o.live_track(),
        click: true,
        dirty: false,
        message,
        take: None,
        editors: Vec::new(),
        to_open: Vec::new(),
        window: None,
        error: None,
        stop_after: o.seconds.map(Duration::from_secs_f64),
        started: Instant::now(),
    };
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
    _midi: Option<midi::Connection>,
}

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
}

impl Gui {
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
        let remote = Remote::new(mixer, from_gui, Arc::clone(&self.status));
        let stream = output.start(remote, Some(from_midi))?;
        self.recorded = Some(recorded);
        self.audio = Some(Audio {
            _stream: stream,
            commands,
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
        }
    }

    fn act(&mut self, action: Action) {
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
            Action::Save => self.save(),
        }
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
        self.session.add_clip(self.live, start, &events);
        self.dirty = true;
        self.message = format!(
            "recorded {} events into a new clip on track {}",
            events.len(),
            self.live + 1
        );
        self.rebuild();
    }

    fn save(&mut self) {
        let Some(file) = self.save_to.clone() else {
            self.message = "start refrain gui with --save <project> to choose where to save".into();
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
