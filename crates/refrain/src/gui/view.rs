//! What the main window shows, and what can be done in it.
//!
//! [`show`] draws a [`Model`] and returns the [`Action`]s the person took.
//! It changes nothing itself, so it can be tested without a window, a device
//! or plugins.

use egui::{
    Align, Align2, Color32, FontId, Layout, Pos2, Rect, Sense, Stroke, Ui, UiBuilder, vec2,
};
use refrain_engine::{Event, TICKS_PER_BEAT, Tempo};
use refrain_project::{Mix, Note, Track};

use super::piano::{self, PianoView};
use crate::plugin::Choice;
use crate::session::{Chain, Slot};

/// Everything the window shows.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Model {
    pub title: String,
    pub tracks: Vec<TrackView>,
    pub master_volume_db: f32,
    pub master_effects: Vec<String>,
    /// The track MIDI goes to.
    pub live: usize,
    pub playing: bool,
    pub recording: bool,
    /// Whether there is an audio output to play on.
    pub can_play: bool,
    /// Seconds from the start of the timeline.
    pub playhead: f64,
    pub tempo: f64,
    pub click: bool,
    /// Unsaved changes.
    pub dirty: bool,
    /// The latest thing worth telling, such as an error.
    pub status: String,
    /// Choosing a plugin to add, when that is going on.
    pub chooser: Option<ChooserView>,
    /// The clip picked in the timeline: its track and its index there.
    pub selected: Option<(usize, usize)>,
    /// The clip whose notes are being edited.
    pub piano: Option<PianoView>,
    pub can_undo: bool,
    pub can_redo: bool,
}

/// What a plugin being chosen is for.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Purpose {
    /// The instrument of a new track.
    Track,
    /// An effect at the end of a chain.
    Effect(Chain),
}

/// The plugin chooser: plugin files to pick from, and once one is picked,
/// the plugins it holds.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ChooserView {
    pub purpose: Option<Purpose>,
    /// Shows only files whose path contains this, ignoring case.
    pub filter: String,
    pub files: Vec<FileView>,
    /// The file picked, by its index in `files`.
    pub file: Option<usize>,
    pub plugins: Vec<Choice>,
    /// What is typed as a path.
    pub path: String,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct FileView {
    pub path: String,
    /// Already used by this project, rather than found where plugins are
    /// installed.
    pub in_project: bool,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct TrackView {
    pub name: String,
    pub instrument: String,
    pub effects: Vec<String>,
    pub mix: Mix,
    pub clips: Vec<ClipView>,
}

/// A clip, in seconds from the start of the timeline.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ClipView {
    pub start: f64,
    pub end: f64,
    pub notes: Vec<NoteView>,
    /// Where it starts, in ticks of the timeline.
    pub start_tick: u64,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct NoteView {
    pub start: f64,
    pub end: f64,
    pub key: u8,
}

/// What the person asked for.
#[derive(Clone, Debug, PartialEq)]
pub enum Action {
    Play,
    Stop,
    Record,
    ToStart,
    /// Move the playhead to these seconds.
    Locate(f64),
    Click(bool),
    Tempo(f64),
    LiveTrack(usize),
    Mix(usize, Mix),
    MasterVolume(f32),
    OpenEditor(Slot),
    Save,
    SaveAs,
    New,
    Open,
    /// Start choosing a plugin for this.
    Choose(Purpose),
    Filter(String),
    /// What is typed in the chooser's path field.
    TypePath(String),
    /// A file of the chooser's list, by index.
    PickFile(usize),
    /// A file named by its path, typed or chosen in the system's dialog
    /// (`None`).
    PickPath(Option<String>),
    /// A plugin of the file picked, by index.
    PickPlugin(usize),
    CloseChooser,
    RemoveTrack(usize),
    RemoveEffect(Chain, usize),
    /// Pick a clip by track and index, or none.
    SelectClip(Option<(usize, usize)>),
    /// Move a clip to start at a tick.
    MoveClip(usize, usize, u64),
    DuplicateClip(usize, usize),
    DeleteClip(usize, usize),
    /// Edit a clip's notes in the piano roll.
    OpenPiano(usize, usize),
    ClosePiano,
    /// A clip's notes, all of them, after an edit.
    SetNotes(usize, usize, Vec<Note>),
    Undo,
    Redo,
}

impl TrackView {
    pub fn new(track: &Track, tempo: Tempo, instrument: String, effects: Vec<String>) -> Self {
        Self {
            name: track.name.clone(),
            instrument,
            effects,
            mix: track.mix,
            clips: clips_of(track, tempo),
        }
    }
}

/// A track's clips, with their notes paired up: each note on with the next
/// note off of its key. A note left on ends with its clip.
pub fn clips_of(track: &Track, tempo: Tempo) -> Vec<ClipView> {
    track
        .clips
        .iter()
        .map(|clip| {
            let at = |tick: u64| tempo.seconds_at(clip.start + tick);
            let end = at(clip.events.last().map_or(0, |e| e.0));
            let mut notes = Vec::new();
            let mut sounding: Vec<(u8, u8, f64)> = Vec::new();
            for (tick, midi) in &clip.events {
                match Event::from_midi(midi) {
                    Some(Event::NoteOn { channel, key, .. }) => {
                        sounding.push((channel, key, at(*tick)));
                    }
                    Some(Event::NoteOff { channel, key, .. }) => {
                        if let Some(i) = sounding
                            .iter()
                            .position(|&(c, k, _)| (c, k) == (channel, key))
                        {
                            let (_, key, start) = sounding.remove(i);
                            notes.push(NoteView {
                                start,
                                end: at(*tick),
                                key,
                            });
                        }
                    }
                    _ => {}
                }
            }
            notes.extend(
                sounding
                    .into_iter()
                    .map(|(_, key, start)| NoteView { start, end, key }),
            );
            ClipView {
                start: at(0),
                end,
                notes,
                start_tick: clip.start,
            }
        })
        .collect()
}

const HEADER_WIDTH: f32 = 420.0;
const ROW_HEIGHT: f32 = 84.0;
const RULER_HEIGHT: f32 = 22.0;
const BEATS_PER_BAR: f64 = 4.0;

impl Model {
    /// How much of the timeline to show: the clips and a little after, and
    /// never less than 16 seconds.
    fn length(&self) -> f64 {
        let end = self
            .tracks
            .iter()
            .flat_map(|t| &t.clips)
            .map(|c| c.end)
            .fold(self.playhead, f64::max);
        (end + 4.0).max(16.0)
    }

    fn seconds_per_bar(&self) -> f64 {
        60.0 / self.tempo * BEATS_PER_BAR
    }

    /// The playhead as bar and beat, counted from 1, and as minutes and
    /// seconds.
    pub fn position(&self) -> String {
        let beat = (self.playhead / (60.0 / self.tempo)).floor() as u64;
        let bar = beat / BEATS_PER_BAR as u64 + 1;
        let minutes = (self.playhead / 60.0).floor();
        let seconds = self.playhead - minutes * 60.0;
        format!(
            "bar {bar} beat {}   {minutes}:{seconds:06.3}",
            beat % BEATS_PER_BAR as u64 + 1
        )
    }
}

/// Draws the window and returns what the person did.
pub fn show(ui: &mut Ui, model: &Model) -> Vec<Action> {
    let mut actions = Vec::new();
    shortcuts(ui, model, &mut actions);
    egui::Panel::top("transport").show(ui, |ui| transport(ui, model, &mut actions));
    egui::Panel::bottom("master").show(ui, |ui| master(ui, model, &mut actions));
    egui::CentralPanel::default_margins().show(ui, |ui| {
        egui::ScrollArea::vertical().show(ui, |ui| timeline(ui, model, &mut actions));
    });
    actions
}

/// Keys that act on the selected clip, and undo and redo, unless something
/// such as a text field takes the keyboard.
fn shortcuts(ui: &mut Ui, model: &Model, actions: &mut Vec<Action>) {
    use egui::{Key, Modifiers};
    if ui.ctx().egui_wants_keyboard_input() {
        return;
    }
    ui.input_mut(|i| {
        if i.consume_key(Modifiers::COMMAND | Modifiers::SHIFT, Key::Z) {
            actions.push(Action::Redo);
        }
        if i.consume_key(Modifiers::COMMAND, Key::Z) {
            actions.push(Action::Undo);
        }
        if let Some((t, c)) = model.selected {
            if i.consume_key(Modifiers::NONE, Key::Delete)
                || i.consume_key(Modifiers::NONE, Key::Backspace)
            {
                actions.push(Action::DeleteClip(t, c));
            }
            if i.consume_key(Modifiers::COMMAND, Key::D) {
                actions.push(Action::DuplicateClip(t, c));
            }
        }
    });
}

fn transport(ui: &mut Ui, model: &Model, actions: &mut Vec<Action>) {
    ui.horizontal(|ui| {
        let stopped = !model.playing;
        if ui
            .add_enabled(model.can_play, egui::Button::new("To start"))
            .clicked()
        {
            actions.push(Action::ToStart);
        }
        let (label, action) = if model.playing {
            ("Stop", Action::Stop)
        } else {
            ("Play", Action::Play)
        };
        if ui
            .add_enabled(model.can_play, egui::Button::new(label))
            .clicked()
        {
            actions.push(action);
        }
        let record = egui::Button::new("Record").selected(model.recording);
        if ui
            .add_enabled(
                model.can_play && stopped && !model.tracks.is_empty(),
                record,
            )
            .clicked()
        {
            actions.push(Action::Record);
        }
        let mut click = model.click;
        if ui.checkbox(&mut click, "Click").changed() {
            actions.push(Action::Click(click));
        }
        ui.label("Tempo");
        let mut tempo = model.tempo;
        let drag = egui::DragValue::new(&mut tempo)
            .range(20.0..=300.0)
            .speed(0.5)
            .suffix(" bpm");
        if ui.add_enabled(stopped, drag).changed() {
            actions.push(Action::Tempo(tempo));
        }
        ui.monospace(model.position());
        ui.separator();
        for (label, action) in [("New", Action::New), ("Open…", Action::Open)] {
            if ui.add_enabled(stopped, egui::Button::new(label)).clicked() {
                actions.push(action);
            }
        }
        let save = if model.dirty { "Save*" } else { "Save" };
        if ui.button(save).clicked() {
            actions.push(Action::Save);
        }
        if ui.button("Save as…").clicked() {
            actions.push(Action::SaveAs);
        }
        ui.separator();
        if ui
            .add_enabled(model.can_undo, egui::Button::new("Undo"))
            .clicked()
        {
            actions.push(Action::Undo);
        }
        if ui
            .add_enabled(model.can_redo, egui::Button::new("Redo"))
            .clicked()
        {
            actions.push(Action::Redo);
        }
        ui.separator();
        if ui
            .add_enabled(stopped, egui::Button::new("Add track"))
            .clicked()
        {
            actions.push(Action::Choose(Purpose::Track));
        }
        ui.label(&model.status);
    });
}

fn master(ui: &mut Ui, model: &Model, actions: &mut Vec<Action>) {
    ui.horizontal(|ui| {
        ui.strong("Master");
        let mut db = model.master_volume_db;
        let slider = egui::Slider::new(&mut db, -60.0..=12.0)
            .suffix(" dB")
            .text("volume");
        if ui.add(slider).changed() {
            actions.push(Action::MasterVolume(db));
        }
        for (k, name) in model.master_effects.iter().enumerate() {
            effect_button(ui, model, name, Chain::Master, k, actions);
        }
        add_effect_button(ui, model, Chain::Master, actions);
    });
    if let Some(chooser) = &model.chooser {
        chooser_window(ui, chooser, actions);
    }
    if let Some(piano) = &model.piano {
        piano::window(ui, piano, actions);
    }
}

/// An effect's button: it opens the editor, and its menu removes it.
fn effect_button(
    ui: &mut Ui,
    model: &Model,
    name: &str,
    chain: Chain,
    k: usize,
    actions: &mut Vec<Action>,
) {
    let slot = match chain {
        Chain::Track(t) => Slot::Effect(t, k),
        Chain::Master => Slot::Master(k),
    };
    let response = ui
        .button(name)
        .on_hover_text("open its editor; right-click for more");
    if response.clicked() {
        actions.push(Action::OpenEditor(slot));
    }
    response.context_menu(|ui| {
        if ui
            .add_enabled(!model.playing, egui::Button::new("Remove effect"))
            .clicked()
        {
            actions.push(Action::RemoveEffect(chain, k));
        }
    });
}

fn add_effect_button(ui: &mut Ui, model: &Model, chain: Chain, actions: &mut Vec<Action>) {
    let add = egui::Button::new("+ effect");
    if ui
        .add_enabled(!model.playing, add)
        .on_hover_text("add an effect at the end")
        .clicked()
    {
        actions.push(Action::Choose(Purpose::Effect(chain)));
    }
}

/// The plugin chooser, in a window of its own over the main one.
fn chooser_window(ui: &mut Ui, chooser: &ChooserView, actions: &mut Vec<Action>) {
    let title = match chooser.purpose {
        Some(Purpose::Track) | None => "Choose an instrument for a new track",
        Some(Purpose::Effect(Chain::Master)) => "Choose an effect for the master",
        Some(Purpose::Effect(Chain::Track(_))) => "Choose an effect for the track",
    };
    egui::Window::new(title)
        .collapsible(false)
        .anchor(Align2::CENTER_CENTER, vec2(0.0, 0.0))
        .default_size(vec2(560.0, 420.0))
        .show(ui.ctx(), |ui| {
            let mut filter = chooser.filter.clone();
            ui.horizontal(|ui| {
                ui.label("Find");
                let edit = egui::TextEdit::singleline(&mut filter).hint_text("part of a path");
                if ui.add(edit).changed() {
                    actions.push(Action::Filter(filter.clone()));
                }
            });
            let needle = chooser.filter.to_lowercase();
            egui::ScrollArea::vertical()
                .max_height(200.0)
                .id_salt("files")
                .show(ui, |ui| {
                    let shown = chooser
                        .files
                        .iter()
                        .enumerate()
                        .filter(|(_, f)| f.path.to_lowercase().contains(&needle));
                    let mut any = false;
                    for (i, file) in shown {
                        any = true;
                        let label = if file.in_project {
                            format!("{} (in this project)", file.path)
                        } else {
                            file.path.clone()
                        };
                        if ui
                            .selectable_label(chooser.file == Some(i), label)
                            .clicked()
                        {
                            actions.push(Action::PickFile(i));
                        }
                    }
                    if !any {
                        ui.weak("No plugin files found here.");
                    }
                });
            ui.horizontal(|ui| {
                let mut path = chooser.path.clone();
                let edit = egui::TextEdit::singleline(&mut path)
                    .hint_text("or a path to a .clap or .vst3");
                if ui.add(edit).changed() {
                    actions.push(Action::TypePath(path));
                }
                let typed = !chooser.path.is_empty();
                if ui.add_enabled(typed, egui::Button::new("Use")).clicked() {
                    actions.push(Action::PickPath(Some(chooser.path.clone())));
                }
                if ui.button("Other file…").clicked() {
                    actions.push(Action::PickPath(None));
                }
            });
            if !chooser.plugins.is_empty() {
                ui.separator();
                ui.label("Plugins in this file:");
                for (i, plugin) in chooser.plugins.iter().enumerate() {
                    let kind = if plugin.effect {
                        "effect"
                    } else {
                        "instrument"
                    };
                    if ui.button(format!("{}  ({kind})", plugin.name)).clicked() {
                        actions.push(Action::PickPlugin(i));
                    }
                }
            }
            ui.separator();
            if ui.button("Cancel").clicked() {
                actions.push(Action::CloseChooser);
            }
        });
}

/// The ruler and one row per track: its header, then its lane of clips.
/// Every lane starts at the same place as the ruler, so clips line up with
/// the bars above them.
fn timeline(ui: &mut Ui, model: &Model, actions: &mut Vec<Action>) {
    let length = model.length();
    let width = ui.available_width();
    // A row's header and lane, side by side.
    let split = |row: Rect| {
        let header = Rect::from_min_size(row.min, vec2(HEADER_WIDTH - 8.0, row.height()));
        let lane = Rect::from_min_max(Pos2::new(row.left() + HEADER_WIDTH, row.top()), row.max);
        (header, lane)
    };
    let (row, _) = ui.allocate_exact_size(vec2(width, RULER_HEIGHT), Sense::hover());
    let (_, rect) = split(row);
    let response = ui.interact(rect, ui.scope_id().with("ruler"), Sense::click());
    ruler(ui, model, rect, length);
    locate_on_click(&response, rect, length, actions);
    if model.tracks.is_empty() {
        ui.label("This project has no tracks yet. Add one with `refrain track add`.");
    }
    for (t, track) in model.tracks.iter().enumerate() {
        let (row, _) = ui.allocate_exact_size(vec2(width, ROW_HEIGHT), Sense::hover());
        let (header_rect, rect) = split(row);
        ui.scope_builder(
            UiBuilder::new()
                .max_rect(header_rect)
                .layout(Layout::top_down(Align::LEFT)),
            |ui| header(ui, model, t, track, actions),
        );
        let response = ui.interact(rect, ui.scope_id().with(("lane", t)), Sense::click());
        lane(ui, model, t, track, rect, length, actions);
        locate_on_click(&response, rect, length, actions);
        ui.separator();
    }
}

fn header(ui: &mut Ui, model: &Model, t: usize, track: &TrackView, actions: &mut Vec<Action>) {
    ui.spacing_mut().slider_width = 100.0;
    ui.horizontal(|ui| {
        if ui
            .radio(model.live == t, "MIDI")
            .on_hover_text("MIDI and recording go to this track")
            .clicked()
            && model.live != t
        {
            actions.push(Action::LiveTrack(t));
        }
        let name = ui
            .add(egui::Label::new(egui::RichText::new(&track.name).strong()).sense(Sense::click()))
            .on_hover_text("right-click for more");
        name.context_menu(|ui| {
            if ui
                .add_enabled(!model.playing, egui::Button::new("Remove track"))
                .clicked()
            {
                actions.push(Action::RemoveTrack(t));
            }
        });
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            let mut mix = track.mix;
            if ui.toggle_value(&mut mix.solo, "S").changed() {
                actions.push(Action::Mix(t, mix));
            }
            if ui.toggle_value(&mut mix.mute, "M").changed() {
                actions.push(Action::Mix(t, mix));
            }
        });
    });
    ui.horizontal(|ui| {
        let mut mix = track.mix;
        let volume = egui::Slider::new(&mut mix.volume_db, -60.0..=12.0)
            .suffix(" dB")
            .text("volume");
        let pan = egui::Slider::new(&mut mix.pan, -1.0..=1.0).text("pan");
        if ui.add(volume).changed() | ui.add(pan).changed() {
            actions.push(Action::Mix(t, mix));
        }
    });
    ui.horizontal_wrapped(|ui| {
        if ui
            .button(&track.instrument)
            .on_hover_text("open its editor")
            .clicked()
        {
            actions.push(Action::OpenEditor(Slot::Instrument(t)));
        }
        for (k, name) in track.effects.iter().enumerate() {
            effect_button(ui, model, name, Chain::Track(t), k, actions);
        }
        add_effect_button(ui, model, Chain::Track(t), actions);
    });
}

fn locate_on_click(response: &egui::Response, rect: Rect, length: f64, actions: &mut Vec<Action>) {
    if response.clicked()
        && let Some(pos) = response.interact_pointer_pos()
    {
        let seconds = f64::from((pos.x - rect.left()) / rect.width()) * length;
        actions.push(Action::Locate(seconds.max(0.0)));
    }
}

fn x_at(rect: Rect, length: f64, seconds: f64) -> f32 {
    rect.left() + (seconds / length) as f32 * rect.width()
}

fn ruler(ui: &Ui, model: &Model, rect: Rect, length: f64) {
    let painter = ui.painter_at(rect);
    let visuals = ui.visuals();
    painter.rect_filled(rect, 0.0, visuals.faint_bg_color);
    let bar = model.seconds_per_bar();
    let mut n = 0;
    while f64::from(n) * bar <= length {
        let x = x_at(rect, length, f64::from(n) * bar);
        painter.line_segment(
            [Pos2::new(x, rect.top()), Pos2::new(x, rect.bottom())],
            Stroke::new(1.0, visuals.weak_text_color()),
        );
        painter.text(
            Pos2::new(x + 3.0, rect.center().y),
            Align2::LEFT_CENTER,
            (n + 1).to_string(),
            FontId::proportional(11.0),
            visuals.text_color(),
        );
        n += 1;
    }
    playhead(ui, model, rect, length);
}

/// How far `response`'s widget has been dragged: while it is, and on the
/// frame it is let go, when egui no longer says.
pub(super) fn drag_offset(ui: &Ui, response: &egui::Response) -> egui::Vec2 {
    let id = response.id.with("drag offset");
    if let Some(delta) = response.total_drag_delta() {
        ui.memory_mut(|m| m.data.insert_temp(id, delta));
        return delta;
    }
    if response.drag_stopped() {
        return ui
            .memory_mut(|m| m.data.remove_temp::<egui::Vec2>(id))
            .unwrap_or_default();
    }
    egui::Vec2::ZERO
}

/// The ticks a horizontal drag of `dx` stands for in a lane `width` wide
/// showing `length` seconds, to the nearest beat.
fn beats_dragged(dx: f32, width: f32, length: f64, tempo: f64) -> i64 {
    let seconds = f64::from(dx / width) * length;
    (seconds * tempo / 60.0).round() as i64 * TICKS_PER_BEAT as i64
}

#[allow(clippy::too_many_arguments)]
fn lane(
    ui: &mut Ui,
    model: &Model,
    t: usize,
    track: &TrackView,
    rect: Rect,
    length: f64,
    actions: &mut Vec<Action>,
) {
    let painter = ui.painter_at(rect);
    let visuals = ui.visuals().clone();
    painter.rect_filled(rect, 0.0, visuals.extreme_bg_color);
    let bar = model.seconds_per_bar();
    let mut n = 0;
    while f64::from(n) * bar <= length {
        let x = x_at(rect, length, f64::from(n) * bar);
        painter.line_segment(
            [Pos2::new(x, rect.top()), Pos2::new(x, rect.bottom())],
            Stroke::new(1.0, visuals.faint_bg_color),
        );
        n += 1;
    }
    let keys = track.clips.iter().flat_map(|c| &c.notes).map(|n| n.key);
    let (low, high) = keys.fold((127u8, 0u8), |(l, h), k| (l.min(k), h.max(k)));
    let span = f32::from(high.saturating_sub(low).max(12));
    let colour = if track.mix.mute {
        Color32::from_gray(110)
    } else {
        Color32::from_rgb(70, 140, 200)
    };
    let seconds_per_tick = 60.0 / model.tempo / TICKS_PER_BEAT as f64;
    for (c, clip) in track.clips.iter().enumerate() {
        let clip_rect = Rect::from_x_y_ranges(
            x_at(rect, length, clip.start)
                ..=x_at(rect, length, clip.end).max(x_at(rect, length, clip.start) + 6.0),
            rect.top() + 4.0..=rect.bottom() - 4.0,
        );
        let response = ui.interact(
            clip_rect,
            ui.scope_id().with(("clip", t, c)),
            Sense::click_and_drag(),
        );
        let label = format!("clip {} on track {}", c + 1, t + 1);
        response
            .widget_info(|| egui::WidgetInfo::labeled(egui::accesskit::Role::Button, true, &label));
        // While dragged, the clip is drawn where it would land.
        let mut shift = 0.0;
        if response.dragged() || response.drag_stopped() {
            let dx = drag_offset(ui, &response).x;
            let ticks = beats_dragged(dx, rect.width(), length, model.tempo);
            let start = (clip.start_tick as i64 + ticks).max(0) as u64;
            shift = (start as f64 - clip.start_tick as f64) * seconds_per_tick;
            if response.drag_stopped() && start != clip.start_tick {
                actions.push(Action::MoveClip(t, c, start));
            }
        }
        if response.clicked() {
            actions.push(Action::SelectClip(Some((t, c))));
        }
        if response.double_clicked() {
            actions.push(Action::OpenPiano(t, c));
        }
        response.context_menu(|ui| {
            if ui.button("Edit notes").clicked() {
                actions.push(Action::OpenPiano(t, c));
            }
            if ui.button("Duplicate").clicked() {
                actions.push(Action::DuplicateClip(t, c));
            }
            if ui.button("Delete clip").clicked() {
                actions.push(Action::DeleteClip(t, c));
            }
        });
        let at = |seconds: f64| x_at(rect, length, seconds + shift);
        let shown = Rect::from_x_y_ranges(
            at(clip.start)..=at(clip.end).max(at(clip.start) + 6.0),
            clip_rect.y_range(),
        );
        let stroke = if model.selected == Some((t, c)) {
            Stroke::new(2.0, visuals.strong_text_color())
        } else {
            Stroke::new(1.0, colour)
        };
        painter.rect_filled(shown, 3.0, colour.gamma_multiply(0.35));
        painter.rect_stroke(shown, 3.0, stroke, egui::StrokeKind::Inside);
        let inner = shown.shrink(4.0);
        for note in &clip.notes {
            let y = inner.bottom() - f32::from(note.key - low) / span * inner.height();
            let x0 = at(note.start);
            let x1 = at(note.end).max(x0 + 2.0);
            painter.line_segment(
                [Pos2::new(x0, y), Pos2::new(x1, y)],
                Stroke::new(3.0, colour),
            );
        }
    }
    playhead(ui, model, rect, length);
}

fn playhead(ui: &Ui, model: &Model, rect: Rect, length: f64) {
    let x = x_at(rect, length, model.playhead);
    let colour = if model.recording {
        Color32::from_rgb(220, 60, 60)
    } else {
        ui.visuals().strong_text_color()
    };
    ui.painter_at(rect).line_segment(
        [Pos2::new(x, rect.top()), Pos2::new(x, rect.bottom())],
        Stroke::new(2.0, colour),
    );
}

#[cfg(test)]
mod tests {
    use egui_kittest::Harness;
    use egui_kittest::kittest::Queryable;
    use refrain_project::Project;

    use super::*;

    /// The window for the mix project kept for CI: two tracks of the phrase.
    fn model() -> Model {
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../tests/projects/mix-clap.refrain"
        );
        let project = Project::read(std::path::Path::new(path)).unwrap();
        let tempo = project.tempo();
        Model {
            title: "mix".into(),
            tracks: project
                .tracks
                .iter()
                .map(|t| {
                    let effects = t.effects.iter().map(|e| e.name.clone()).collect();
                    TrackView::new(t, tempo, t.instrument.name.clone(), effects)
                })
                .collect(),
            master_volume_db: 0.0,
            master_effects: vec!["refrain test effect".into()],
            live: 0,
            playing: false,
            recording: false,
            can_play: true,
            playhead: 0.5,
            tempo: project.tempo,
            click: true,
            dirty: false,
            status: String::new(),
            chooser: None,
            selected: None,
            piano: None,
            can_undo: false,
            can_redo: false,
        }
    }

    type State = (Model, Vec<Action>);

    /// The window, drawn without a GPU unless `wgpu`: clicking needs none.
    fn harness_with(model: Model, wgpu: bool) -> Harness<'static, State> {
        let mut builder = Harness::builder()
            .with_size(vec2(1_200.0, 600.0))
            .with_context_setup(|ctx| crate::gui::fonts::install(ctx).unwrap());
        if wgpu {
            builder = builder.wgpu();
        }
        let mut harness = builder.build_ui_state(
            |ui, (model, actions): &mut State| actions.extend(show(ui, model)),
            (model, Vec::new()),
        );
        harness.run();
        harness.state_mut().1.clear();
        harness
    }

    fn harness(model: Model) -> Harness<'static, State> {
        harness_with(model, false)
    }

    fn after_click(model: Model, label: &str) -> Vec<Action> {
        let mut harness = harness(model);
        harness.get_by_label(label).click();
        harness.run();
        harness.state().1.clone()
    }

    #[test]
    fn the_transport_buttons_ask_for_what_they_say() {
        assert_eq!(after_click(model(), "Play"), [Action::Play]);
        assert_eq!(after_click(model(), "To start"), [Action::ToStart]);
        assert_eq!(after_click(model(), "Record"), [Action::Record]);
        assert_eq!(after_click(model(), "Save"), [Action::Save]);
        assert_eq!(after_click(model(), "Click"), [Action::Click(false)]);
        let playing = Model {
            playing: true,
            ..model()
        };
        assert_eq!(after_click(playing, "Stop"), [Action::Stop]);
    }

    #[test]
    fn nothing_plays_without_an_output_and_recording_waits_for_a_stop() {
        let silent = Model {
            can_play: false,
            ..model()
        };
        assert!(after_click(silent.clone(), "Play").is_empty());
        assert!(after_click(silent, "Record").is_empty());
        let playing = Model {
            playing: true,
            ..model()
        };
        assert!(after_click(playing, "Record").is_empty());
    }

    #[test]
    fn a_tracks_header_mixes_it_and_opens_its_plugins() {
        let model = model();
        let mut harness = harness(model.clone());
        harness.get_all_by_label("M").nth(1).unwrap().click();
        harness.run();
        let muted = Mix {
            mute: true,
            ..model.tracks[1].mix
        };
        assert_eq!(harness.state().1, [Action::Mix(1, muted)]);

        harness.state_mut().1.clear();
        harness.get_all_by_label("MIDI").nth(1).unwrap().click();
        harness.run();
        assert_eq!(harness.state().1, [Action::LiveTrack(1)]);

        harness.state_mut().1.clear();
        // Track 2's effect and the master's have the same name.
        for i in 0..2 {
            let effects: Vec<_> = harness.get_all_by_label("refrain test effect").collect();
            assert_eq!(effects.len(), 2);
            effects[i].click();
            harness.run();
        }
        let mut opened = std::mem::take(&mut harness.state_mut().1);
        opened.sort_by_key(|a| format!("{a:?}"));
        assert_eq!(
            opened,
            [
                Action::OpenEditor(Slot::Effect(1, 0)),
                Action::OpenEditor(Slot::Master(0))
            ]
        );
        harness
            .get_all_by_role_and_label(egui::accesskit::Role::Button, "refrain test synth")
            .next()
            .unwrap()
            .click();
        harness.run();
        assert_eq!(harness.state().1, [Action::OpenEditor(Slot::Instrument(0))]);
    }

    #[test]
    fn clips_show_their_notes_where_they_were_played() {
        let model = model();
        let clip = &model.tracks[0].clips[0];
        assert_eq!(clip.start, 0.0);
        // The demo phrase: C, E, G, C one after another, then a chord.
        assert_eq!(clip.notes.len(), 7);
        let first = clip.notes[0];
        assert_eq!(first.key, 60);
        assert!((first.start - 0.0).abs() < 1e-9 && (first.end - 0.15).abs() < 1e-3);
        assert!((clip.end - 1.8).abs() < 1e-3, "{}", clip.end);
        assert_eq!(model.position(), "bar 1 beat 2   0:00.500");
    }

    #[test]
    fn project_buttons_ask_for_what_they_say() {
        assert_eq!(after_click(model(), "New"), [Action::New]);
        assert_eq!(after_click(model(), "Open…"), [Action::Open]);
        assert_eq!(after_click(model(), "Save as…"), [Action::SaveAs]);
        assert_eq!(
            after_click(model(), "Add track"),
            [Action::Choose(Purpose::Track)]
        );
        // Not while playing: the plugins would have to stop.
        let playing = Model {
            playing: true,
            ..model()
        };
        assert!(after_click(playing.clone(), "Add track").is_empty());
        assert!(after_click(playing, "New").is_empty());
    }

    /// Clicks every widget with `label`, one after the other, and returns
    /// what was asked for, sorted.
    fn click_all(harness: &mut Harness<'static, State>, label: &str) -> Vec<Action> {
        let count = harness.get_all_by_label(label).count();
        for i in 0..count {
            harness.get_all_by_label(label).nth(i).unwrap().click();
            harness.run();
        }
        let mut actions = std::mem::take(&mut harness.state_mut().1);
        actions.sort_by_key(|a| format!("{a:?}"));
        actions
    }

    #[test]
    fn effects_are_added_at_the_end_of_a_track_or_the_master() {
        let mut harness = harness(model());
        assert_eq!(
            click_all(&mut harness, "+ effect"),
            [
                Action::Choose(Purpose::Effect(Chain::Master)),
                Action::Choose(Purpose::Effect(Chain::Track(0))),
                Action::Choose(Purpose::Effect(Chain::Track(1))),
            ]
        );
    }

    #[test]
    fn menus_remove_tracks_and_effects() {
        use egui::accesskit::Role;
        let mut harness = harness(model());
        harness
            .get_all_by_role_and_label(Role::Label, "refrain test synth")
            .nth(1)
            .unwrap()
            .click_secondary();
        harness.run();
        harness.get_by_label("Remove track").click();
        harness.run();
        assert_eq!(harness.state().1, [Action::RemoveTrack(1)]);

        let mut harness = self::harness(model());
        // The master's effect comes first.
        harness
            .get_all_by_label("refrain test effect")
            .nth(1)
            .unwrap()
            .click_secondary();
        harness.run();
        harness.get_by_label("Remove effect").click();
        harness.run();
        assert_eq!(
            harness.state().1,
            [Action::RemoveEffect(Chain::Track(1), 0)]
        );
    }

    fn choosing() -> Model {
        Model {
            chooser: Some(ChooserView {
                purpose: Some(Purpose::Effect(Chain::Track(0))),
                files: vec![
                    FileView {
                        path: "plugins/synth.clap".into(),
                        in_project: true,
                    },
                    FileView {
                        path: "plugins/Delay.vst3".into(),
                        in_project: false,
                    },
                ],
                ..ChooserView::default()
            }),
            ..model()
        }
    }

    #[test]
    fn the_chooser_picks_files_and_then_plugins() {
        assert_eq!(
            after_click(choosing(), "plugins/Delay.vst3"),
            [Action::PickFile(1)]
        );
        assert_eq!(
            after_click(choosing(), "plugins/synth.clap (in this project)"),
            [Action::PickFile(0)]
        );
        assert_eq!(after_click(choosing(), "Cancel"), [Action::CloseChooser]);
        assert_eq!(
            after_click(choosing(), "Other file…"),
            [Action::PickPath(None)]
        );

        // A filter hides what does not match.
        let mut filtered = choosing();
        filtered.chooser.as_mut().unwrap().filter = "DELAY".into();
        let harness = harness(filtered);
        assert!(harness.query_by_label("plugins/Delay.vst3").is_some());
        assert!(
            harness
                .query_by_label("plugins/synth.clap (in this project)")
                .is_none()
        );

        // A typed path is used as it is.
        let mut typed = choosing();
        typed.chooser.as_mut().unwrap().path = "my/Reverb.clap".into();
        assert_eq!(
            after_click(typed, "Use"),
            [Action::PickPath(Some("my/Reverb.clap".into()))]
        );

        // Once a file is picked, its plugins are there to pick.
        let mut picked = choosing();
        let chooser = picked.chooser.as_mut().unwrap();
        chooser.file = Some(0);
        chooser.plugins = vec![
            Choice {
                id: "e".into(),
                name: "refrain test effect".into(),
                effect: true,
            },
            Choice {
                id: "s".into(),
                name: "refrain test synth".into(),
                effect: false,
            },
        ];
        assert_eq!(
            after_click(picked, "refrain test synth  (instrument)"),
            [Action::PickPlugin(1)]
        );
    }

    #[test]
    fn clips_are_picked_and_their_menus_act_on_them() {
        assert_eq!(
            after_click(model(), "clip 1 on track 2"),
            [Action::SelectClip(Some((1, 0)))]
        );
        for (item, action) in [
            ("Duplicate", Action::DuplicateClip(1, 0)),
            ("Delete clip", Action::DeleteClip(1, 0)),
            ("Edit notes", Action::OpenPiano(1, 0)),
        ] {
            let mut harness = harness(model());
            harness.get_by_label("clip 1 on track 2").click_secondary();
            harness.run();
            harness.get_by_label(item).click();
            harness.run();
            assert_eq!(harness.state().1, [action], "{item}");
        }
    }

    /// Drags the widget labelled `label` by `by`, and returns what was asked.
    fn drag(model: Model, label: &str, by: egui::Vec2) -> Vec<Action> {
        let mut harness = harness(model);
        let from = harness.get_by_label(label).rect().center();
        harness.drag_at(from);
        harness.run();
        for step in 1..=4 {
            harness.hover_at(from + by * (step as f32 / 4.0));
            harness.run();
        }
        harness.drop_at(from + by);
        harness.run();
        std::mem::take(&mut harness.state_mut().1)
    }

    #[test]
    fn a_dragged_clip_moves_by_whole_beats() {
        let actions = drag(model(), "clip 1 on track 1", vec2(150.0, 0.0));
        let moved: Vec<_> = actions
            .iter()
            .filter(|a| matches!(a, Action::MoveClip(..)))
            .collect();
        match moved.as_slice() {
            [Action::MoveClip(0, 0, start)] => {
                assert!(*start > 0 && start % TICKS_PER_BEAT == 0, "{start}");
            }
            other => panic!("{other:?}"),
        }
        // Dragged left of the start, it stays where it is.
        let actions = drag(model(), "clip 1 on track 1", vec2(-150.0, 0.0));
        assert!(!actions.iter().any(|a| matches!(a, Action::MoveClip(..))));
        assert_eq!(
            beats_dragged(100.0, 800.0, 16.0, 120.0),
            4 * TICKS_PER_BEAT as i64
        );
    }

    #[test]
    fn keys_act_on_the_picked_clip_and_undo() {
        use egui::{Key, Modifiers};
        let picked = Model {
            selected: Some((0, 0)),
            can_undo: true,
            can_redo: true,
            ..model()
        };
        let pressed = |modifiers: Modifiers, key: Key| {
            let mut harness = harness(picked.clone());
            harness.key_press_modifiers(modifiers, key);
            harness.run();
            std::mem::take(&mut harness.state_mut().1)
        };
        assert_eq!(
            pressed(Modifiers::NONE, Key::Delete),
            [Action::DeleteClip(0, 0)]
        );
        assert_eq!(
            pressed(Modifiers::NONE, Key::Backspace),
            [Action::DeleteClip(0, 0)]
        );
        assert_eq!(
            pressed(Modifiers::COMMAND, Key::D),
            [Action::DuplicateClip(0, 0)]
        );
        assert_eq!(pressed(Modifiers::COMMAND, Key::Z), [Action::Undo]);
        assert_eq!(
            pressed(Modifiers::COMMAND | Modifiers::SHIFT, Key::Z),
            [Action::Redo]
        );
        assert_eq!(after_click(picked.clone(), "Undo"), [Action::Undo]);
        // Nothing to undo: the buttons do nothing.
        assert!(after_click(model(), "Undo").is_empty());
        assert!(after_click(model(), "Redo").is_empty());
    }

    /// The notes of the mix project's first clip.
    fn model_notes() -> Vec<Note> {
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../tests/projects/mix-clap.refrain"
        );
        let project = Project::read(std::path::Path::new(path)).unwrap();
        project.tracks[0].clips[0].notes().0
    }

    fn with_piano(notes: Vec<Note>) -> Model {
        Model {
            piano: Some(PianoView {
                track: 0,
                clip: 0,
                notes,
                length: 4 * 4 * TICKS_PER_BEAT,
            }),
            ..model()
        }
    }

    fn c4() -> Note {
        Note {
            start: 0,
            length: TICKS_PER_BEAT,
            channel: 0,
            key: 60,
            velocity: 100,
            release: 64,
        }
    }

    #[test]
    fn the_piano_roll_adds_removes_moves_and_resizes_notes() {
        let mut harness = harness(with_piano(vec![c4()]));
        harness.get_by_label("note 60 at tick 0").click_secondary();
        harness.run();
        assert_eq!(harness.state().1, [Action::SetNotes(0, 0, Vec::new())]);

        let added = after_click(with_piano(vec![c4()]), "notes");
        match added.as_slice() {
            [Action::SetNotes(0, 0, notes)] => {
                assert_eq!(notes.len(), 2);
                assert!(notes.contains(&c4()));
                let new = notes.iter().find(|n| **n != c4()).unwrap();
                assert_eq!(new.length, piano::NEW_LENGTH);
                assert_eq!(new.start % piano::SNAP, 0);
            }
            other => panic!("{other:?}"),
        }

        // A beat is 80 points wide and a key 10 high.
        let moved = drag(
            with_piano(vec![c4()]),
            "note 60 at tick 0",
            vec2(160.0, -20.0),
        );
        let expected = Note {
            start: 2 * TICKS_PER_BEAT,
            key: 62,
            ..c4()
        };
        assert_eq!(moved.last(), Some(&Action::SetNotes(0, 0, vec![expected])));
        let longer = drag(
            with_piano(vec![c4()]),
            "end of note 60 at tick 0",
            vec2(80.0, 0.0),
        );
        let expected = Note {
            length: 2 * TICKS_PER_BEAT,
            ..c4()
        };
        assert_eq!(longer.last(), Some(&Action::SetNotes(0, 0, vec![expected])));
    }

    /// Writes what the window looks like to target/gui-preview.png, for a
    /// person to look at: `cargo test -p refrain preview -- --ignored`.
    #[test]
    #[ignore = "writes a picture; run by hand"]
    fn preview() {
        let target = concat!(env!("CARGO_MANIFEST_DIR"), "/../../target");
        let piano = Model {
            selected: Some((0, 0)),
            ..with_piano(model_notes())
        };
        for (model, name) in [
            (model(), "gui-preview.png"),
            (choosing(), "gui-chooser.png"),
            (piano, "gui-piano.png"),
        ] {
            let mut harness = harness_with(model, true);
            let image = harness.render().unwrap();
            image.save(format!("{target}/{name}")).unwrap();
        }
    }
}
