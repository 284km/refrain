//! A project with all its plugins loaded.

use std::path::{Path, PathBuf};

use refrain_engine::{Event, Mixer, Processor, Tempo, Track as MixerTrack};
use refrain_project::{Plugin as Stored, Project, State};

use crate::Result;
use crate::plugin::Kind;

/// Which plugin of a session: counted from 0.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Slot {
    Instrument(usize),
    Effect(usize, usize),
    Master(usize),
}

/// A project, and the plugins it names, loaded with their states.
pub struct Session {
    pub project: Project,
    // Where the project's relative plugin paths start from.
    file: PathBuf,
    tracks: Vec<LoadedTrack>,
    master: Vec<Kind>,
}

struct LoadedTrack {
    instrument: Kind,
    effects: Vec<Kind>,
}

fn is_project(path: &Path) -> bool {
    path.extension()
        .is_some_and(|e| e.eq_ignore_ascii_case(refrain_project::EXTENSION))
}

impl Session {
    /// Loads a `.refrain` project with its plugins and their states, or a
    /// single `.clap` or `.vst3` plugin as a project of one track. `id`
    /// chooses the plugin in a plugin file.
    ///
    /// # Safety
    ///
    /// This runs the plugins' code in this process.
    pub unsafe fn open(path: &Path, id: Option<&str>) -> Result<Self> {
        if !is_project(path) {
            // Paths start from the current folder until the project is saved.
            let file = PathBuf::from("untitled.refrain");
            let kind = unsafe { Kind::open(path, id) }?;
            let (format, id, name) = kind.identity();
            let stored = Stored::new(&file, format, path, id, name, State::default());
            return Ok(Self {
                project: Project::with_instrument(stored),
                file,
                tracks: vec![LoadedTrack {
                    instrument: kind,
                    effects: Vec::new(),
                }],
                master: Vec::new(),
            });
        }
        let project = Project::read(path)?;
        let load = |stored: &Stored| -> Result<Kind> {
            let file = stored.file(path);
            let failed = |e: Box<dyn std::error::Error>| {
                format!(
                    "{}: {}, at {}: {e}",
                    path.display(),
                    stored.name,
                    file.display()
                )
            };
            let mut kind = unsafe { Kind::open(&file, Some(&stored.id)) }.map_err(failed)?;
            kind.load_state(&stored.state).map_err(failed)?;
            Ok(kind)
        };
        let mut tracks = Vec::new();
        for track in &project.tracks {
            tracks.push(LoadedTrack {
                instrument: load(&track.instrument)?,
                effects: track.effects.iter().map(load).collect::<Result<_>>()?,
            });
        }
        let master = project
            .master
            .effects
            .iter()
            .map(load)
            .collect::<Result<_>>()?;
        let session = Self {
            project,
            file: path.to_path_buf(),
            tracks,
            master,
        };
        println!("{}", session.summary());
        Ok(session)
    }

    /// One line about the project.
    fn summary(&self) -> String {
        let p = &self.project;
        let count = |n: usize, what: &str| match n {
            1 => format!("1 {what}"),
            n => format!("{n} {what}s"),
        };
        let clips = p.tracks.iter().map(|t| t.clips.len()).sum();
        let effects =
            p.tracks.iter().map(|t| t.effects.len()).sum::<usize>() + p.master.effects.len();
        format!(
            "project {}: {}, {}, {}, at {} beats per minute",
            self.file.display(),
            count(p.tracks.len(), "track"),
            count(effects, "effect"),
            count(clips, "clip"),
            p.tempo
        )
    }

    /// The plugin in `slot`, or an error naming what is missing.
    pub fn plugin(&mut self, slot: Slot) -> Result<&mut Kind> {
        let tracks = self.tracks.len();
        let missing = || -> Box<dyn std::error::Error> {
            match slot {
                Slot::Instrument(t) | Slot::Effect(t, _) if t >= tracks => {
                    format!("there is no track {}; the project has {tracks}", t + 1).into()
                }
                Slot::Effect(t, e) => format!("track {} has no effect {}", t + 1, e + 1).into(),
                Slot::Master(e) => format!("the master has no effect {}", e + 1).into(),
                Slot::Instrument(_) => unreachable!(),
            }
        };
        let plugin = match slot {
            Slot::Instrument(t) => self.tracks.get_mut(t).map(|t| &mut t.instrument),
            Slot::Effect(t, e) => self.tracks.get_mut(t).and_then(|t| t.effects.get_mut(e)),
            Slot::Master(e) => self.master.get_mut(e),
        };
        plugin.ok_or_else(missing)
    }

    fn plugins_mut(&mut self) -> impl Iterator<Item = &mut Kind> {
        self.tracks
            .iter_mut()
            .flat_map(|t| std::iter::once(&mut t.instrument).chain(&mut t.effects))
            .chain(&mut self.master)
    }

    /// Answers every plugin's requests for the main thread.
    pub fn poll_main_thread(&mut self) {
        self.plugins_mut().for_each(Kind::poll_main_thread);
    }

    pub fn tempo(&self) -> Tempo {
        self.project.tempo()
    }

    pub fn set_tempo(&mut self, beats_per_minute: f64) {
        self.project.tempo = beats_per_minute;
    }

    /// The frame just after the last event of any clip, at `sample_rate`.
    pub fn sequence_end(&self, sample_rate: f32) -> u64 {
        let tempo = self.tempo();
        self.project
            .tracks
            .iter()
            .filter_map(|t| t.sequence().last().map(|e| e.0))
            .map(|tick| tempo.frame_at(tick, sample_rate) + 1)
            .max()
            .unwrap_or(0)
    }

    /// Tells every plugin whether it renders offline. Call before
    /// [`mixer`](Self::mixer). Returns how many took the mode, and how many
    /// were told.
    pub fn set_offline(&mut self, offline: bool) -> (usize, usize) {
        let mut took = 0;
        let mut told = 0;
        for p in self.plugins_mut() {
            took += usize::from(p.set_offline(offline));
            told += 1;
        }
        (took, told)
    }

    /// Checks that track `index` exists.
    pub fn check_track(&self, index: usize) -> Result<()> {
        match self.tracks.len() {
            0 => Err("the project has no tracks; add one with `refrain track add`".into()),
            n if index >= n => {
                Err(format!("there is no track {}; the project has {n}", index + 1).into())
            }
            _ => Ok(()),
        }
    }

    /// Activates every plugin and puts them together: each track's
    /// instrument, playing the track's clips, then its effects, mixed, then
    /// the master's effects. Plugins activated before are activated again,
    /// so the last mixer has to be gone. Live events go to track `live`, and so do
    /// `extra` events, at frames from the start.
    pub fn mixer(
        &mut self,
        sample_rate: f32,
        max_block_frames: usize,
        live: usize,
        extra: &[(u64, Event)],
    ) -> Result<Mixer> {
        let tempo = self.tempo();
        let mut tracks = Vec::new();
        for (t, (track, loaded)) in self.project.tracks.iter().zip(&mut self.tracks).enumerate() {
            let mut sequence: Vec<(u64, Event)> = track
                .sequence()
                .into_iter()
                .map(|(tick, event)| (tempo.frame_at(tick, sample_rate), event))
                .collect();
            if t == live {
                sequence.extend_from_slice(extra);
            }
            let mut chain: Vec<Box<dyn Processor>> =
                vec![loaded.instrument.activate(sample_rate, max_block_frames)?];
            for effect in &mut loaded.effects {
                chain.push(effect.activate(sample_rate, max_block_frames)?);
            }
            tracks.push(MixerTrack::new(chain, track.mix.to_engine()).with_sequence(sequence));
        }
        let master = self
            .master
            .iter_mut()
            .map(|e| e.activate(sample_rate, max_block_frames))
            .collect::<Result<_>>()?;
        Ok(Mixer::new(tracks, master, self.project.master.volume_db).with_live_track(live))
    }

    /// Adds a clip to track `track` at tick `start`, holding `events` at
    /// ticks from there.
    pub fn add_clip(&mut self, track: usize, start: u64, events: &[(u64, Event)]) {
        self.project.tracks[track].add_clip(start, events);
    }

    /// Writes the project, with every plugin's current state, to `file`.
    pub fn save(&mut self, file: &Path) -> Result<()> {
        let mut states = Vec::new();
        for plugin in self.plugins_mut() {
            states.push(plugin.save_state()?);
        }
        for (stored, state) in self.project.plugins_mut().zip(states) {
            stored.state = state;
        }
        self.project.move_to(&self.file, file);
        self.project.write(file)?;
        self.file = file.to_path_buf();
        Ok(())
    }
}
