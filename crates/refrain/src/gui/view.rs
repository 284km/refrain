//! What the main window shows, and what can be done in it.
//!
//! [`show`] draws a [`Model`] and returns the [`Action`]s the person took.
//! It changes nothing itself, so it can be tested without a window, a device
//! or plugins.

use egui::{
    Align, Align2, Color32, FontId, Layout, Pos2, Rect, Sense, Stroke, Ui, UiBuilder, vec2,
};
use refrain_engine::{Event, Tempo};
use refrain_project::{Mix, Track};

use crate::session::Slot;

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
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct NoteView {
    pub start: f64,
    pub end: f64,
    pub key: u8,
}

/// What the person asked for.
#[derive(Clone, Copy, Debug, PartialEq)]
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
    egui::Panel::top("transport").show(ui, |ui| transport(ui, model, &mut actions));
    egui::Panel::bottom("master").show(ui, |ui| master(ui, model, &mut actions));
    egui::CentralPanel::default_margins().show(ui, |ui| {
        egui::ScrollArea::vertical().show(ui, |ui| timeline(ui, model, &mut actions));
    });
    actions
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
        let save = if model.dirty { "Save*" } else { "Save" };
        if ui.button(save).clicked() {
            actions.push(Action::Save);
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
            if ui.button(name).on_hover_text("open its editor").clicked() {
                actions.push(Action::OpenEditor(Slot::Master(k)));
            }
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
        lane(ui, model, track, rect, length);
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
        ui.strong(&track.name);
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
            if ui.button(name).on_hover_text("open its editor").clicked() {
                actions.push(Action::OpenEditor(Slot::Effect(t, k)));
            }
        }
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

fn lane(ui: &Ui, model: &Model, track: &TrackView, rect: Rect, length: f64) {
    let painter = ui.painter_at(rect);
    let visuals = ui.visuals();
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
    for clip in &track.clips {
        let clip_rect = Rect::from_x_y_ranges(
            x_at(rect, length, clip.start)
                ..=x_at(rect, length, clip.end).max(x_at(rect, length, clip.start) + 4.0),
            rect.top() + 4.0..=rect.bottom() - 4.0,
        );
        painter.rect_filled(clip_rect, 3.0, colour.gamma_multiply(0.35));
        painter.rect_stroke(
            clip_rect,
            3.0,
            Stroke::new(1.0, colour),
            egui::StrokeKind::Inside,
        );
        let inner = clip_rect.shrink(4.0);
        for note in &clip.notes {
            let y = inner.bottom() - f32::from(note.key - low) / span * inner.height();
            let x0 = x_at(rect, length, note.start);
            let x1 = x_at(rect, length, note.end).max(x0 + 2.0);
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

    /// Writes what the window looks like to target/gui-preview.png, for a
    /// person to look at: `cargo test -p refrain preview -- --ignored`.
    #[test]
    #[ignore = "writes a picture; run by hand"]
    fn preview() {
        let mut harness = harness_with(model(), true);
        let image = harness.render().unwrap();
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../target/gui-preview.png");
        image.save(path).unwrap();
    }
}
