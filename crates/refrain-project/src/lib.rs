//! refrain's project files.
//!
//! A project holds tracks. Each track has an instrument plugin, a chain of
//! effect plugins after it, how it goes into the mix, and the clips played on
//! it. The mixed tracks go through the master's effects. Every plugin is
//! named with its state, so opening the project brings the sound back. It is
//! JSON, so it can be read and diffed:
//!
//! ```json
//! {
//!   "refrain": 3,
//!   "tempo": 120.0,
//!   "ticks_per_beat": 960,
//!   "tracks": [
//!     {
//!       "name": "Synth",
//!       "instrument": {
//!         "format": "vst3",
//!         "path": "plugins/Synth.vst3",
//!         "id": "7E5C0A312B9D4F1A9C3E6D521F0A8B44",
//!         "name": "Synth",
//!         "state": { "data": "cnRzMQAAAD8=", "controller": "" }
//!       },
//!       "effects": [
//!         {
//!           "format": "clap",
//!           "path": "plugins/Delay.clap",
//!           "id": "com.example.delay",
//!           "name": "Delay",
//!           "state": { "data": "", "controller": "" }
//!         }
//!       ],
//!       "mix": { "volume_db": -6.0, "pan": 0.25, "mute": false, "solo": false },
//!       "clips": [
//!         { "start": 0, "events": [[0, [144, 60, 100]], [480, [128, 60, 64]]] }
//!       ]
//!     }
//!   ],
//!   "master": { "effects": [], "volume_db": 0.0 }
//! }
//! ```
//!
//! A clip starts at a tick of the timeline, and its events are at ticks from
//! the clip's start, each a MIDI 1.0 message. `effects`, `mix`, `clips` and
//! `master` may be left out. Versions 1 and 2 held one instrument and its
//! clips; they read as a project with that one track. Version 1 had no tempo
//! and no clips; it reads as 120 beats per minute with no clips.
//!
//! States are base64. `data` is a CLAP plugin's state or a VST3 component's;
//! `controller` is a VST3 controller's, and empty for CLAP. An empty `data`
//! means the plugin starts as it would on its own. A relative `path` is
//! relative to the project file's folder, so a project can be moved together
//! with the plugins next to it.

use std::error::Error;
use std::fmt;
use std::path::{Component, Path, PathBuf};

use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64;
use refrain_engine::{Event, TICKS_PER_BEAT, Tempo};
use serde::{Deserialize, Serialize};

/// The file extension of projects.
pub const EXTENSION: &str = "refrain";

/// The version of the format this crate writes. It reads this and older ones.
pub const VERSION: u32 = 3;

/// The quietest and loudest volumes a project may hold, in decibels.
pub const MIN_VOLUME_DB: f32 = -120.0;
pub const MAX_VOLUME_DB: f32 = 24.0;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(try_from = "Stored")]
pub struct Project {
    /// The format version.
    pub refrain: u32,
    /// Beats per minute.
    pub tempo: f64,
    pub ticks_per_beat: u64,
    pub tracks: Vec<Track>,
    pub master: Master,
}

/// A project as stored, in any version this crate reads.
#[derive(Deserialize)]
struct Stored {
    refrain: u32,
    #[serde(default = "default_tempo")]
    tempo: f64,
    #[serde(default = "default_ticks_per_beat")]
    ticks_per_beat: u64,
    #[serde(default)]
    tracks: Vec<Track>,
    #[serde(default)]
    master: Master,
    // Versions 1 and 2: one instrument, and the clips played on it.
    instrument: Option<Plugin>,
    #[serde(default)]
    clips: Vec<Clip>,
}

impl TryFrom<Stored> for Project {
    type Error = String;

    fn try_from(stored: Stored) -> Result<Self, String> {
        let mut tracks = stored.tracks;
        match (stored.refrain, stored.instrument) {
            (0..=2, Some(instrument)) if tracks.is_empty() => {
                let mut track = Track::new(instrument);
                track.clips = stored.clips;
                tracks.push(track);
            }
            (0..=2, _) => return Err("a version 1 or 2 project names one instrument".into()),
            (_, None) if stored.clips.is_empty() => {}
            _ => {
                return Err(format!(
                    "a version {} project keeps instruments and clips in tracks",
                    stored.refrain
                ));
            }
        }
        Ok(Self {
            refrain: stored.refrain,
            tempo: stored.tempo,
            ticks_per_beat: stored.ticks_per_beat,
            tracks,
            master: stored.master,
        })
    }
}

fn default_tempo() -> f64 {
    120.0
}

fn default_ticks_per_beat() -> u64 {
    TICKS_PER_BEAT
}

/// An instrument, the effects after it, how it is mixed, and what is played
/// on it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Track {
    /// For people: the instrument's name, unless someone changes it.
    pub name: String,
    pub instrument: Plugin,
    /// In order: each takes what the one before gives.
    #[serde(default)]
    pub effects: Vec<Plugin>,
    #[serde(default)]
    pub mix: Mix,
    #[serde(default)]
    pub clips: Vec<Clip>,
}

impl Track {
    /// A track with `instrument`, named after it, and nothing else.
    pub fn new(instrument: Plugin) -> Self {
        Self {
            name: instrument.name.clone(),
            instrument,
            effects: Vec::new(),
            mix: Mix::default(),
            clips: Vec::new(),
        }
    }

    /// Every clip's events at ticks of the timeline, in order.
    pub fn sequence(&self) -> Vec<(u64, Event)> {
        let mut events: Vec<(u64, Event)> = self
            .clips
            .iter()
            .flat_map(|clip| {
                clip.events.iter().filter_map(move |(tick, midi)| {
                    Some((clip.start + tick, Event::from_midi(midi)?))
                })
            })
            .collect();
        events.sort_by_key(|e| e.0);
        events
    }

    /// Adds a clip at `start`, holding `events` at ticks from the start.
    pub fn add_clip(&mut self, start: u64, events: &[(u64, Event)]) {
        let mut events: Vec<(u64, Vec<u8>)> = events
            .iter()
            .map(|(tick, event)| (*tick, midi_bytes(event)))
            .collect();
        events.sort_by_key(|e| e.0);
        self.clips.push(Clip { start, events });
    }
}

/// How a track goes into the mix.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Mix {
    /// Volume in decibels; 0 leaves the track as it is.
    pub volume_db: f32,
    /// Balance from left (-1) to right (1).
    pub pan: f32,
    pub mute: bool,
    /// When any track is soloed, only soloed tracks are heard.
    pub solo: bool,
}

impl Mix {
    pub fn to_engine(self) -> refrain_engine::Mix {
        refrain_engine::Mix {
            volume_db: self.volume_db,
            pan: self.pan,
            mute: self.mute,
            solo: self.solo,
        }
    }
}

/// The effects every track goes through once mixed, and the volume after
/// them.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Master {
    pub effects: Vec<Plugin>,
    pub volume_db: f32,
}

/// Events played on an instrument, from a tick of the timeline.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Clip {
    /// The tick of the timeline the clip starts at.
    pub start: u64,
    /// Ticks from the clip's start, and MIDI 1.0 messages, in order.
    pub events: Vec<(u64, Vec<u8>)>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Format {
    Clap,
    Vst3,
}

/// A plugin, where to find it, and its state.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Plugin {
    pub format: Format,
    /// Where the plugin file is, as stored: `/` separates folders on every
    /// platform, and a relative path is relative to the project's folder.
    pub path: String,
    /// The plugin's ID: a CLAP plugin ID, or a VST3 class ID.
    pub id: String,
    /// The plugin's name, for people reading the file.
    pub name: String,
    pub state: State,
}

impl Plugin {
    /// The plugin at `plugin` (relative to the current folder, or absolute),
    /// for a project to be written to `file`.
    pub fn new(
        file: &Path,
        format: Format,
        plugin: &Path,
        id: String,
        name: String,
        state: State,
    ) -> Self {
        Self {
            format,
            path: store_path(plugin, file),
            id,
            name,
            state,
        }
    }

    /// Where the plugin file is, for a project read from `file`.
    pub fn file(&self, file: &Path) -> PathBuf {
        let stored = PathBuf::from(&self.path);
        if stored.is_absolute() {
            stored
        } else {
            fold_parents(&folder_of(file).join(stored))
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct State {
    #[serde(with = "base64_bytes")]
    pub data: Vec<u8>,
    #[serde(with = "base64_bytes", default)]
    pub controller: Vec<u8>,
}

mod base64_bytes {
    use super::*;

    pub fn serialize<S: serde::Serializer>(bytes: &[u8], s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&BASE64.encode(bytes))
    }

    pub fn deserialize<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Vec<u8>, D::Error> {
        let text = String::deserialize(d)?;
        BASE64.decode(text).map_err(serde::de::Error::custom)
    }
}

#[derive(Debug)]
pub enum ProjectError {
    Io(PathBuf, std::io::Error),
    Parse(PathBuf, serde_json::Error),
    /// Written by a newer refrain.
    Version(u32),
    /// Readable, but not a valid project. Says why.
    Invalid(PathBuf, String),
}

impl fmt::Display for ProjectError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(path, e) => write!(f, "{}: {e}", path.display()),
            Self::Parse(path, e) => write!(f, "{} is not a refrain project: {e}", path.display()),
            Self::Version(v) => write!(
                f,
                "the project is version {v}; this refrain reads up to version {VERSION}"
            ),
            Self::Invalid(path, why) => write!(f, "{}: {why}", path.display()),
        }
    }
}

impl Error for ProjectError {}

impl Default for Project {
    fn default() -> Self {
        Self {
            refrain: VERSION,
            tempo: default_tempo(),
            ticks_per_beat: TICKS_PER_BEAT,
            tracks: Vec::new(),
            master: Master::default(),
        }
    }
}

impl Project {
    /// A project with one track, playing `instrument`.
    pub fn with_instrument(instrument: Plugin) -> Self {
        Self {
            tracks: vec![Track::new(instrument)],
            ..Self::default()
        }
    }

    pub fn tempo(&self) -> Tempo {
        Tempo::new(self.tempo)
    }

    /// The tick just after the last event of any clip on any track.
    pub fn end(&self) -> u64 {
        self.tracks
            .iter()
            .filter_map(|t| t.sequence().last().map(|e| e.0 + 1))
            .max()
            .unwrap_or(0)
    }

    /// Every plugin: each track's instrument and effects, then the master's
    /// effects.
    pub fn plugins_mut(&mut self) -> impl Iterator<Item = &mut Plugin> {
        self.tracks
            .iter_mut()
            .flat_map(|t| std::iter::once(&mut t.instrument).chain(&mut t.effects))
            .chain(&mut self.master.effects)
    }

    /// Makes the project, read from `from`, ready to be written to `to`:
    /// relative plugin paths are changed to lead to the same files from
    /// there.
    pub fn move_to(&mut self, from: &Path, to: &Path) {
        for plugin in self.plugins_mut() {
            let file = plugin.file(from);
            plugin.path = store_path(&file, to);
        }
    }

    pub fn read(file: &Path) -> Result<Self, ProjectError> {
        let text = std::fs::read_to_string(file).map_err(|e| ProjectError::Io(file.into(), e))?;
        let parse = |e| ProjectError::Parse(file.into(), e);
        // A newer version may not parse as this one; say what it is.
        let value: serde_json::Value = serde_json::from_str(&text).map_err(parse)?;
        if let Some(version) = value.get("refrain").and_then(serde_json::Value::as_u64)
            && version > u64::from(VERSION)
        {
            return Err(ProjectError::Version(
                version.try_into().unwrap_or(u32::MAX),
            ));
        }
        let project: Project = serde_json::from_value(value).map_err(parse)?;
        project
            .check()
            .map_err(|why| ProjectError::Invalid(file.into(), why))?;
        Ok(project)
    }

    /// Why the project is not one refrain can play, if it is not.
    fn check(&self) -> Result<(), String> {
        if self.ticks_per_beat != TICKS_PER_BEAT {
            return Err(format!("ticks_per_beat must be {TICKS_PER_BEAT}"));
        }
        if !(self.tempo.is_finite() && (1.0..=1_000.0).contains(&self.tempo)) {
            return Err(format!("tempo {} is not between 1 and 1000", self.tempo));
        }
        let volume = |db: f32, what: &str| {
            if (MIN_VOLUME_DB..=MAX_VOLUME_DB).contains(&db) {
                Ok(())
            } else {
                Err(format!(
                    "{what}: volume_db {db} is not between {MIN_VOLUME_DB} and {MAX_VOLUME_DB}"
                ))
            }
        };
        for (t, track) in self.tracks.iter().enumerate() {
            let what = format!("track {}", t + 1);
            volume(track.mix.volume_db, &what)?;
            if !(-1.0..=1.0).contains(&track.mix.pan) {
                return Err(format!(
                    "{what}: pan {} is not between -1 and 1",
                    track.mix.pan
                ));
            }
            for (c, clip) in track.clips.iter().enumerate() {
                if let Some((tick, midi)) = clip
                    .events
                    .iter()
                    .find(|(_, m)| Event::from_midi(m).is_none())
                {
                    return Err(format!(
                        "{what}, clip {}: tick {tick}: {midi:?} is not a MIDI message refrain reads",
                        c + 1
                    ));
                }
            }
        }
        volume(self.master.volume_db, "master")
    }

    /// Writes the project to `file`, in the current version of the format,
    /// replacing the file only once the new contents are complete. A project
    /// that could not be read back is refused.
    pub fn write(&self, file: &Path) -> Result<(), ProjectError> {
        self.check()
            .map_err(|why| ProjectError::Invalid(file.into(), why))?;
        // A project read from an older version may have gained what that
        // version cannot hold; an older refrain must not read it as its own.
        let current = Project {
            refrain: VERSION,
            ..self.clone()
        };
        let mut text = serde_json::to_string_pretty(&current).expect("projects serialize");
        text.push('\n');
        let partial = file.with_extension("refrain-partial");
        std::fs::write(&partial, text).map_err(|e| ProjectError::Io(partial.clone(), e))?;
        std::fs::rename(&partial, file).map_err(|e| ProjectError::Io(file.into(), e))
    }
}

/// A MIDI message's bytes: one status byte and its data, without padding.
fn midi_bytes(event: &Event) -> Vec<u8> {
    let bytes = event.to_midi();
    let length = if matches!(event, Event::ChannelPressure { .. }) {
        2
    } else {
        3
    };
    bytes[..length].to_vec()
}

/// How to store `plugin` in a project written to `file`: absolute paths stay
/// absolute; relative ones become relative to the project's folder.
fn store_path(plugin: &Path, file: &Path) -> String {
    let path = if plugin.is_absolute() {
        plugin.to_path_buf()
    } else {
        relative_to(plugin, folder_of(file)).unwrap_or_else(|| absolute(plugin))
    };
    path.components()
        .map(|c| c.as_os_str().to_string_lossy())
        .collect::<Vec<_>>()
        .join("/")
        .replacen("//", "/", 1)
}

/// The folder `file` is in. A bare file name is in the current folder.
fn folder_of(file: &Path) -> &Path {
    match file.parent() {
        Some(p) if !p.as_os_str().is_empty() => p,
        _ => Path::new("."),
    }
}

/// Folds `folder/..` away, without asking the file system: `songs/../a`
/// becomes `a`. A leading `..` stays.
fn fold_parents(path: &Path) -> PathBuf {
    let mut folded = PathBuf::new();
    for c in path.components() {
        match c {
            Component::CurDir => {}
            Component::ParentDir
                if matches!(folded.components().next_back(), Some(Component::Normal(_))) =>
            {
                folded.pop();
            }
            _ => folded.push(c.as_os_str()),
        }
    }
    folded
}

fn absolute(path: &Path) -> PathBuf {
    std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf())
}

/// `path` as seen from `base`, both relative to the current folder. `None`
/// when one of them leaves the current folder in a way that cannot be
/// followed back.
fn relative_to(path: &Path, base: &Path) -> Option<PathBuf> {
    let path = absolute(path);
    let base = absolute(base);
    let path: Vec<Component> = path
        .components()
        .filter(|c| *c != Component::CurDir)
        .collect();
    let base: Vec<Component> = base
        .components()
        .filter(|c| *c != Component::CurDir)
        .collect();
    if path.iter().chain(&base).any(|c| *c == Component::ParentDir) {
        return None;
    }
    let common = path.iter().zip(&base).take_while(|(a, b)| a == b).count();
    if common == 0 {
        // Different drives on Windows: no relative path exists.
        return None;
    }
    let mut relative = PathBuf::new();
    for _ in common..base.len() {
        relative.push("..");
    }
    for c in &path[common..] {
        relative.push(c.as_os_str());
    }
    Some(relative)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn synth() -> Plugin {
        Plugin {
            format: Format::Vst3,
            path: "plugins/Synth.vst3".into(),
            id: "7E5C0A312B9D4F1A9C3E6D521F0A8B44".into(),
            name: "Synth".into(),
            state: State {
                data: b"rts1\0\0\0\x3f".to_vec(),
                controller: Vec::new(),
            },
        }
    }

    fn sample() -> Project {
        let mut track = Track::new(synth());
        track.effects.push(Plugin {
            format: Format::Clap,
            path: "plugins/Delay.clap".into(),
            id: "com.example.delay".into(),
            name: "Delay".into(),
            state: State::default(),
        });
        track.mix = Mix {
            volume_db: -6.0,
            pan: 0.25,
            mute: false,
            solo: false,
        };
        track.clips.push(Clip {
            start: 0,
            events: vec![(0, vec![144, 60, 100]), (480, vec![128, 60, 64])],
        });
        Project {
            tracks: vec![track],
            ..Project::default()
        }
    }

    /// A directory of its own for one test.
    fn scratch(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("refrain-project-{name}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn reads_what_the_documentation_shows() {
        let doc = include_str!("lib.rs");
        let json: String = doc
            .lines()
            .skip_while(|l| *l != "//! ```json")
            .skip(1)
            .take_while(|l| *l != "//! ```")
            .map(|l| l.strip_prefix("//!").unwrap_or(l))
            .collect::<Vec<_>>()
            .join("\n");
        let project: Project = serde_json::from_str(&json).unwrap();
        assert_eq!(project, sample());
    }

    #[test]
    fn a_version_2_project_reads_as_one_track() {
        let text = r#"{
          "refrain": 2,
          "tempo": 100.0,
          "ticks_per_beat": 960,
          "instrument": {
            "format": "vst3",
            "path": "plugins/Synth.vst3",
            "id": "7E5C0A312B9D4F1A9C3E6D521F0A8B44",
            "name": "Synth",
            "state": { "data": "cnRzMQAAAD8=", "controller": "" }
          },
          "clips": [
            { "start": 0, "events": [[0, [144, 60, 100]], [480, [128, 60, 64]]] }
          ]
        }"#;
        let project: Project = serde_json::from_str(text).unwrap();
        assert_eq!(project.tempo, 100.0);
        assert_eq!(project.tracks.len(), 1);
        let track = &project.tracks[0];
        assert_eq!(track.instrument, synth());
        assert_eq!(track.name, "Synth");
        assert_eq!(track.clips, sample().tracks[0].clips);
        assert!(track.effects.is_empty());
        assert_eq!(track.mix, Mix::default());
        assert_eq!(project.master, Master::default());
    }

    #[test]
    fn a_version_1_project_reads_with_no_clips_at_120() {
        let text = r#"{"refrain": 1, "instrument": {"format": "clap", "path": "a.clap",
            "id": "x", "name": "x", "state": {"data": ""}}}"#;
        let project: Project = serde_json::from_str(text).unwrap();
        assert_eq!(project.tempo, 120.0);
        assert!(project.tracks[0].clips.is_empty());
        // A missing controller state reads as empty.
        assert!(project.tracks[0].instrument.state.controller.is_empty());
    }

    #[test]
    fn versions_and_their_shapes_must_match() {
        let old_without_instrument = r#"{"refrain": 2}"#;
        let new_with_instrument = r#"{"refrain": 3, "instrument": {"format": "clap",
            "path": "a.clap", "id": "x", "name": "x", "state": {"data": ""}}}"#;
        for text in [old_without_instrument, new_with_instrument] {
            assert!(serde_json::from_str::<Project>(text).is_err(), "{text}");
        }
        // A project with no tracks is fine: it is silent.
        let empty: Project = serde_json::from_str(r#"{"refrain": 3}"#).unwrap();
        assert_eq!(empty, Project::default());
    }

    #[test]
    fn an_older_project_is_written_in_the_current_version() {
        let dir = scratch("old");
        let file = dir.join("old.refrain");
        std::fs::write(
            &file,
            r#"{"refrain": 1, "instrument": {"format": "clap", "path": "a.clap",
            "id": "x", "name": "x", "state": {"data": ""}}}"#,
        )
        .unwrap();
        let project = Project::read(&file).unwrap();
        project.write(&file).unwrap();
        let text = std::fs::read_to_string(&file).unwrap();
        assert!(text.contains("\"refrain\": 3"), "{text}");
        assert!(
            !text.contains("\"instrument\": {\n    \"format\""),
            "{text}"
        );
        assert_eq!(Project::read(&file).unwrap().tracks, project.tracks);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn clips_become_one_sequence_in_order() {
        let mut project = sample();
        let on = |key| Event::NoteOn {
            channel: 0,
            key,
            velocity: 100.0 / 127.0,
        };
        let off = |key| Event::NoteOff {
            channel: 0,
            key,
            velocity: 64.0 / 127.0,
        };
        let track = &mut project.tracks[0];
        track.add_clip(240, &[(480, off(64)), (0, on(64))]);
        assert_eq!(
            track.sequence(),
            [(0, on(60)), (240, on(64)), (480, off(60)), (720, off(64))]
        );
        // What a clip stores reads back as the same events.
        assert_eq!(track.clips[1].events[0], (0, vec![0x90, 64, 100]));

        // The end is the latest of any track's.
        assert_eq!(project.end(), 721);
        let mut later = Track::new(synth());
        later.add_clip(960, &[(0, on(67))]);
        project.tracks.push(later);
        assert_eq!(project.end(), 961);
        project.tracks.push(Track::new(synth()));
        assert_eq!(project.end(), 961);
    }

    #[test]
    fn refuses_what_it_cannot_play() {
        let dir = scratch("bad");
        let file = dir.join("bad.refrain");
        let mut broken = Vec::new();
        let mut project = sample();
        project.tracks[0].clips[0].events.push((960, vec![0xc0, 5]));
        broken.push(project);
        let mut project = sample();
        project.tempo = 0.0;
        broken.push(project);
        let mut project = sample();
        project.tracks[0].mix.pan = 1.5;
        broken.push(project);
        let mut project = sample();
        project.tracks[0].mix.volume_db = -200.0;
        broken.push(project);
        let mut project = sample();
        project.master.volume_db = 30.0;
        broken.push(project);
        for project in broken {
            let err = project.write(&file).unwrap_err();
            assert!(matches!(err, ProjectError::Invalid(..)), "{err}");
            assert!(!file.exists());
            // Written some other way, it is refused on reading.
            std::fs::write(&file, serde_json::to_string(&project).unwrap()).unwrap();
            let err = Project::read(&file).unwrap_err();
            assert!(matches!(err, ProjectError::Invalid(..)), "{err}");
            std::fs::remove_file(&file).unwrap();
        }
        // Infinity cannot be written at all.
        let mut project = sample();
        project.tracks[0].mix.volume_db = f32::INFINITY;
        assert!(project.write(&file).is_err());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn writes_and_reads_back() {
        let dir = scratch("rw");
        let file = dir.join("song.refrain");
        let mut project = sample();
        project.master.effects.push(synth());
        project.master.volume_db = -3.0;
        project.write(&file).unwrap();
        assert_eq!(Project::read(&file).unwrap(), project);
        assert!(!file.with_extension("refrain-partial").exists());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn refuses_newer_versions_and_bad_base64() {
        let dir = scratch("v");
        let file = dir.join("new.refrain");
        // A newer version is named as such, even if it no longer parses.
        std::fs::write(&file, r#"{"refrain": 4, "tracks": "something new"}"#).unwrap();
        assert!(matches!(
            Project::read(&file),
            Err(ProjectError::Version(4))
        ));
        std::fs::write(
            &file,
            r#"{"refrain":1,"instrument":{"format":"clap","path":"a",
            "id":"x","name":"x","state":{"data":"!!"}}}"#,
        )
        .unwrap();
        assert!(matches!(Project::read(&file), Err(ProjectError::Parse(..))));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn relative_plugin_paths_follow_the_project() {
        // Both relative to the current folder: the plugin is stored as seen
        // from the project's folder, with / between folders.
        let stored = store_path(
            Path::new("plugins/Synth.vst3"),
            Path::new("songs/a.refrain"),
        );
        assert_eq!(stored, "../plugins/Synth.vst3");
        let plugin = Plugin {
            path: stored,
            ..synth()
        };
        let resolved = plugin.file(Path::new("songs/a.refrain"));
        assert_eq!(resolved, Path::new("plugins/Synth.vst3"));
        assert_eq!(
            fold_parents(Path::new("../a/./b/../c")),
            Path::new("../a/c")
        );

        // Next to each other.
        assert_eq!(
            store_path(Path::new("Synth.clap"), Path::new("a.refrain")),
            "Synth.clap"
        );
    }

    #[test]
    fn moving_a_project_keeps_every_plugin_found() {
        let mut project = sample();
        project.master.effects.push(Plugin {
            path: "Reverb.clap".into(),
            ..synth()
        });
        let absolute_plugin = absolute(Path::new("Elsewhere.vst3"));
        project.tracks[0].effects[0].path = store_path(&absolute_plugin, Path::new("x"));
        let from = Path::new("songs/a.refrain");
        let to = Path::new("backup/old/a.refrain");
        let before: Vec<PathBuf> = project.plugins_mut().map(|p| p.file(from)).collect();
        project.move_to(from, to);
        let after: Vec<PathBuf> = project.plugins_mut().map(|p| p.file(to)).collect();
        assert_eq!(after, before);
        assert_eq!(
            project.tracks[0].instrument.path,
            "../../songs/plugins/Synth.vst3"
        );
        assert_eq!(project.master.effects[0].path, "../../songs/Reverb.clap");
        // Absolute paths stay absolute.
        assert_eq!(project.tracks[0].effects[0].file(to), absolute_plugin);
    }
}
