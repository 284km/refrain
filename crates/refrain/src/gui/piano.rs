//! The piano roll: one clip's notes, on a grid of keys and beats.
//!
//! Click an empty place to add a note, drag a note to move it, drag its end
//! to change its length, and right-click it to remove it. Every change comes
//! back as the clip's whole new list of notes.

use egui::{Align2, Color32, FontId, Pos2, Rect, Sense, Stroke, Ui, vec2};
use refrain_engine::TICKS_PER_BEAT;
use refrain_project::Note;

use super::view::{Action, drag_offset};

/// The clip being edited.
#[derive(Clone, Debug, PartialEq)]
pub struct PianoView {
    pub track: usize,
    pub clip: usize,
    pub notes: Vec<Note>,
    /// How many ticks of the clip to show.
    pub length: u64,
}

const KEY_HEIGHT: f32 = 10.0;
const BEAT_WIDTH: f32 = 80.0;
const KEYBOARD_WIDTH: f32 = 44.0;
/// Notes start and end on sixteenths.
pub const SNAP: u64 = TICKS_PER_BEAT / 4;
/// A new note lasts a beat.
pub const NEW_LENGTH: u64 = TICKS_PER_BEAT;
/// How much of a note's right end resizes it rather than moving it.
const EDGE: f32 = 6.0;

fn x_of(tick: u64) -> f32 {
    tick as f32 / TICKS_PER_BEAT as f32 * BEAT_WIDTH
}

fn y_of(key: u8) -> f32 {
    f32::from(127 - key.min(127)) * KEY_HEIGHT
}

/// The ticks a horizontal distance stands for, to the nearest sixteenth.
fn snapped_ticks(dx: f32) -> i64 {
    let ticks = f64::from(dx / BEAT_WIDTH) * TICKS_PER_BEAT as f64;
    (ticks / SNAP as f64).round() as i64 * SNAP as i64
}

fn is_black(key: u8) -> bool {
    matches!(key % 12, 1 | 3 | 6 | 8 | 10)
}

fn name(key: u8) -> String {
    format!("C{}", i32::from(key / 12) - 1)
}

/// `note` moved by a drag of `delta`: sixteenths across, keys up and down.
fn moved(note: Note, delta: egui::Vec2) -> Note {
    let start = (note.start as i64 + snapped_ticks(delta.x)).max(0) as u64;
    let keys = -(delta.y / KEY_HEIGHT).round() as i32;
    let key = (i32::from(note.key) + keys).clamp(0, 127) as u8;
    Note { start, key, ..note }
}

/// `note` with its end dragged by `dx`, never shorter than a sixteenth.
fn resized(note: Note, dx: f32) -> Note {
    let length = (note.length as i64 + snapped_ticks(dx)).max(SNAP as i64) as u64;
    Note { length, ..note }
}

pub fn window(ui: &mut Ui, piano: &PianoView, actions: &mut Vec<Action>) {
    let title = format!("Notes: track {}, clip {}", piano.track + 1, piano.clip + 1);
    let mut open = true;
    egui::Window::new(title)
        .id(egui::Id::unique("piano roll"))
        .open(&mut open)
        .default_pos(egui::pos2(60.0, 90.0))
        .default_size(vec2(900.0, 440.0))
        .resizable(true)
        .show(ui.ctx(), |ui| {
            ui.weak("Click to add a note, drag it to move it, drag its end to change its length, right-click it to remove it.");
            grid(ui, piano, actions);
        });
    if !open {
        actions.push(Action::ClosePiano);
    }
}

fn grid(ui: &mut Ui, piano: &PianoView, actions: &mut Vec<Action>) {
    let (t, c) = (piano.track, piano.clip);
    let set = |notes: Vec<Note>| Action::SetNotes(t, c, notes);
    // Start scrolled to the notes, or to middle C.
    let middle = if piano.notes.is_empty() {
        60.0
    } else {
        piano.notes.iter().map(|n| f32::from(n.key)).sum::<f32>() / piano.notes.len() as f32
    };
    let first = egui::Id::unique(("piano scrolled", t, c));
    let scrolled = ui.memory(|m| m.data.get_temp::<bool>(first).unwrap_or(false));
    let mut area = egui::ScrollArea::both().auto_shrink(false);
    if !scrolled {
        area = area.vertical_scroll_offset((y_of(middle as u8) - 150.0).max(0.0));
        ui.memory_mut(|m| m.data.insert_temp(first, true));
    }
    area.show(ui, |ui| {
        let size = vec2(KEYBOARD_WIDTH + x_of(piano.length), 128.0 * KEY_HEIGHT);
        let (rect, background) = ui.allocate_exact_size(size, Sense::click());
        background.widget_info(|| {
            egui::WidgetInfo::labeled(egui::accesskit::Role::Canvas, true, "notes")
        });
        let left = rect.left() + KEYBOARD_WIDTH;
        let painter = ui.painter_at(rect);
        let visuals = ui.visuals().clone();
        // Rows for keys, darker for black keys; names at each C.
        for key in 0..=127u8 {
            let y = rect.top() + y_of(key);
            let row = Rect::from_min_size(Pos2::new(rect.left(), y), vec2(size.x, KEY_HEIGHT));
            let fill = if is_black(key) {
                visuals.extreme_bg_color
            } else {
                visuals.faint_bg_color
            };
            painter.rect_filled(row, 0.0, fill);
            if key % 12 == 0 {
                painter.text(
                    Pos2::new(rect.left() + 3.0, y + KEY_HEIGHT / 2.0),
                    Align2::LEFT_CENTER,
                    name(key),
                    FontId::proportional(9.0),
                    visuals.text_color(),
                );
            }
        }
        // Beat lines, stronger on bars.
        let beats = piano.length / TICKS_PER_BEAT + 1;
        for beat in 0..=beats {
            let x = left + x_of(beat * TICKS_PER_BEAT);
            let colour = if beat % 4 == 0 {
                visuals.weak_text_color()
            } else {
                visuals.faint_bg_color.gamma_multiply(2.0)
            };
            painter.line_segment(
                [Pos2::new(x, rect.top()), Pos2::new(x, rect.bottom())],
                Stroke::new(1.0, colour),
            );
        }

        let colour = Color32::from_rgb(70, 140, 200);
        let mut busy = false;
        for (i, &note) in piano.notes.iter().enumerate() {
            let body = Rect::from_min_size(
                Pos2::new(left + x_of(note.start), rect.top() + y_of(note.key)),
                vec2(x_of(note.length).max(3.0), KEY_HEIGHT),
            );
            let edge = Rect::from_min_max(
                Pos2::new((body.right() - EDGE).max(body.center().x), body.top()),
                body.max,
            );
            let move_response = ui.interact(
                body,
                ui.scope_id().with(("note", i)),
                Sense::click_and_drag(),
            );
            let size_response = ui.interact(edge, ui.scope_id().with(("end", i)), Sense::drag());
            let label = format!("note {} at tick {}", note.key, note.start);
            move_response.widget_info(|| {
                egui::WidgetInfo::labeled(egui::accesskit::Role::Button, true, &label)
            });
            size_response.widget_info(|| {
                egui::WidgetInfo::labeled(
                    egui::accesskit::Role::Splitter,
                    true,
                    format!("end of {label}"),
                )
            });
            let mut shown = note;
            if move_response.dragged() || move_response.drag_stopped() {
                shown = moved(note, drag_offset(ui, &move_response));
            }
            if size_response.dragged() || size_response.drag_stopped() {
                shown = resized(note, drag_offset(ui, &size_response).x);
            }
            if move_response.drag_stopped() || size_response.drag_stopped() {
                if shown != note {
                    let mut notes = piano.notes.clone();
                    notes[i] = shown;
                    actions.push(set(notes));
                }
                busy = true;
            }
            if move_response.secondary_clicked() {
                let mut notes = piano.notes.clone();
                notes.remove(i);
                actions.push(set(notes));
                busy = true;
            }
            let shown_rect = Rect::from_min_size(
                Pos2::new(left + x_of(shown.start), rect.top() + y_of(shown.key)),
                vec2(x_of(shown.length).max(3.0), KEY_HEIGHT),
            );
            let fill = colour.gamma_multiply(0.4 + 0.6 * f32::from(note.velocity) / 127.0);
            painter.rect_filled(shown_rect.shrink(0.5), 2.0, fill);
            painter.rect_stroke(
                shown_rect.shrink(0.5),
                2.0,
                Stroke::new(1.0, colour),
                egui::StrokeKind::Inside,
            );
            if size_response.hovered() {
                ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeHorizontal);
            }
        }

        if !busy
            && background.clicked()
            && let Some(pos) = background.interact_pointer_pos()
            && pos.x >= left
        {
            let tick = (pos.x - left) / BEAT_WIDTH * TICKS_PER_BEAT as f32;
            let start = (tick as u64 / SNAP) * SNAP;
            let row = ((pos.y - rect.top()) / KEY_HEIGHT).floor() as i32;
            let key = (127 - row).clamp(0, 127) as u8;
            let mut notes = piano.notes.clone();
            notes.push(Note {
                start,
                length: NEW_LENGTH,
                channel: 0,
                key,
                velocity: 100,
                release: 64,
            });
            notes.sort_by_key(|n| (n.start, n.key));
            actions.push(set(notes));
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn note(start: u64, length: u64, key: u8) -> Note {
        Note {
            start,
            length,
            channel: 0,
            key,
            velocity: 100,
            release: 64,
        }
    }

    #[test]
    fn drags_move_by_sixteenths_and_keys() {
        // A beat is 80 points: a sixteenth is 20.
        let n = note(960, 480, 60);
        assert_eq!(moved(n, vec2(21.0, 0.0)), note(1_200, 480, 60));
        assert_eq!(moved(n, vec2(9.0, 0.0)), n);
        // Up two rows is two keys higher.
        assert_eq!(moved(n, vec2(0.0, -2.0 * KEY_HEIGHT)), note(960, 480, 62));
        // Never before the clip, never off the keyboard.
        assert_eq!(moved(n, vec2(-1_000.0, 0.0)).start, 0);
        assert_eq!(moved(n, vec2(0.0, -10_000.0)).key, 127);
        assert_eq!(resized(n, 40.0).length, 960);
        assert_eq!(resized(n, -1_000.0).length, SNAP);
    }
}
