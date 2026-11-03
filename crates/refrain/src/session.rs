//! A project with all its plugins loaded.

use std::path::{Path, PathBuf};

use refrain_engine::{Event, Mixer, Processor, Tempo, Track as MixerTrack};
use refrain_project::{Plugin as Stored, Project, State, Track};

use crate::Result;
use crate::plugin::Kind;

/// Which plugin of a session: counted from 0.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Slot {
    Instrument(usize),
    Effect(usize, usize),
    Master(usize),
}

/// A chain of effects: a track's, or the master's.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Chain {
    Track(usize),
    Master,
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

    /// A project with no tracks, not saved anywhere yet. Plugin paths start
    /// from the current folder until it is.
    pub fn empty() -> Self {
        Self {
            project: Project::default(),
            file: PathBuf::from("untitled.refrain"),
            tracks: Vec::new(),
            master: Vec::new(),
        }
    }

    /// Names `kind`, loaded from `file`, for this project.
    fn stored(&self, file: &Path, kind: &Kind) -> Stored {
        let (format, id, name) = kind.identity();
        Stored::new(&self.file, format, file, id, name, State::default())
    }

    /// Adds a track playing `kind`, loaded from `file`, and returns its index.
    pub fn add_track(&mut self, file: &Path, kind: Kind) -> usize {
        let stored = self.stored(file, &kind);
        self.project.tracks.push(Track::new(stored));
        self.tracks.push(LoadedTrack {
            instrument: kind,
            effects: Vec::new(),
        });
        self.tracks.len() - 1
    }

    /// Removes track `index`, with its plugins. Their processors and editors
    /// must be gone.
    pub fn remove_track(&mut self, index: usize) -> Result<()> {
        self.check_track(index)?;
        self.project.tracks.remove(index);
        self.tracks.remove(index);
        Ok(())
    }

    fn chain_mut(&mut self, chain: Chain) -> Result<(&mut Vec<Stored>, &mut Vec<Kind>)> {
        match chain {
            Chain::Track(t) => {
                self.check_track(t)?;
                Ok((
                    &mut self.project.tracks[t].effects,
                    &mut self.tracks[t].effects,
                ))
            }
            Chain::Master => Ok((&mut self.project.master.effects, &mut self.master)),
        }
    }

    /// Adds `kind`, loaded from `file`, at the end of `chain`.
    pub fn add_effect(&mut self, chain: Chain, file: &Path, kind: Kind) -> Result<()> {
        let stored = self.stored(file, &kind);
        let (project, loaded) = self.chain_mut(chain)?;
        project.push(stored);
        loaded.push(kind);
        Ok(())
    }

    /// Removes effect `index` of `chain`. Its processor and editor must be
    /// gone.
    pub fn remove_effect(&mut self, chain: Chain, index: usize) -> Result<()> {
        let (project, loaded) = self.chain_mut(chain)?;
        if index >= loaded.len() {
            return Err(format!("there is no effect {} there", index + 1).into());
        }
        project.remove(index);
        loaded.remove(index);
        Ok(())
    }

    /// The plugin files the project uses, each once, as found from here.
    pub fn plugin_files(&self) -> Vec<PathBuf> {
        let mut files: Vec<PathBuf> = Vec::new();
        let project = &self.project;
        let stored = project
            .tracks
            .iter()
            .flat_map(|t| std::iter::once(&t.instrument).chain(&t.effects))
            .chain(&project.master.effects);
        for s in stored {
            let file = s.file(&self.file);
            if !files.contains(&file) {
                files.push(file);
            }
        }
        files
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

#[cfg(test)]
mod tests {
    use clack_host::prelude::PluginEntry;
    use refrain_clap::ClapPlugin;
    use refrain_engine::{TimedEvent, render_offline};
    use refrain_test_tone::effect;
    use refrain_vst3::{Vst3Module, Vst3Plugin};

    use super::*;

    fn clap(id: &str) -> Kind {
        let entry =
            PluginEntry::load_from_clack::<refrain_test_synth::Entry>(c"refrain-test-synth")
                .unwrap();
        Kind::Clap(ClapPlugin::new(&entry, Some(id)).unwrap())
    }

    fn vst3(name: &str) -> Kind {
        let module =
            unsafe { Vst3Module::from_factory(refrain_test_synth_vst3::GetPluginFactory()) }
                .unwrap();
        Kind::Vst3(Vst3Plugin::new(&module, Some(name)).unwrap())
    }

    fn chord() -> Vec<TimedEvent> {
        [60, 64, 67]
            .into_iter()
            .map(|key| TimedEvent {
                frame: 100,
                event: Event::NoteOn {
                    channel: 0,
                    key,
                    velocity: 1.0,
                },
            })
            .collect()
    }

    /// The first channel of what the session plays from `events` on track 0.
    fn play(session: &mut Session) -> Vec<f32> {
        let mut mixer = session.mixer(48_000.0, 128, 0, &[]).unwrap();
        let out = render_offline(&mut mixer, 48_000.0, 1, 4_800, 128, &chord());
        assert!(!mixer.has_failed());
        out
    }

    #[test]
    fn tracks_and_effects_come_and_go_in_step_with_the_project() {
        let mut session = Session::empty();
        assert_eq!(
            session.add_track(Path::new("synth.clap"), clap(refrain_test_synth::ID)),
            0
        );
        let dry = play(&mut session);

        session
            .add_effect(
                Chain::Track(0),
                Path::new("synth.vst3"),
                vst3(refrain_test_synth_vst3::EFFECT_NAME),
            )
            .unwrap();
        session
            .add_effect(
                Chain::Master,
                Path::new("synth.clap"),
                clap(refrain_test_synth::EFFECT_ID),
            )
            .unwrap();
        // The track's effect, then the master's: doubled and clipped twice.
        let mut expected = dry.clone();
        effect::process(effect::DEFAULT_GAIN, &mut expected);
        effect::process(effect::DEFAULT_GAIN, &mut expected);
        assert_eq!(play(&mut session), expected);

        let p = &session.project;
        assert_eq!(
            p.tracks[0].effects[0].name,
            refrain_test_synth_vst3::EFFECT_NAME
        );
        assert_eq!(p.master.effects[0].id, refrain_test_synth::EFFECT_ID);
        assert_eq!(
            session.plugin_files(),
            [PathBuf::from("synth.clap"), PathBuf::from("synth.vst3")]
        );

        // A second track, which MIDI does not reach, adds nothing.
        session.add_track(Path::new("synth.vst3"), vst3(refrain_test_synth_vst3::NAME));
        assert_eq!(play(&mut session), expected);

        session.remove_effect(Chain::Track(0), 0).unwrap();
        session.remove_effect(Chain::Master, 0).unwrap();
        assert_eq!(play(&mut session), dry);
        session.remove_track(0).unwrap();
        assert_eq!(session.project.tracks.len(), 1);
        assert_eq!(
            session.project.tracks[0].instrument.name,
            refrain_test_synth_vst3::NAME
        );

        assert!(session.remove_track(1).is_err());
        assert!(session.remove_effect(Chain::Track(0), 0).is_err());
        assert!(session.remove_effect(Chain::Master, 0).is_err());
    }

    #[test]
    fn a_saved_session_keeps_each_plugins_state_in_its_place() {
        let mut session = Session::empty();
        session.add_track(Path::new("synth.clap"), clap(refrain_test_synth::ID));
        let mut half = clap(refrain_test_synth::EFFECT_ID);
        half.load_state(&State {
            data: effect::save_state(0.5).to_vec(),
            controller: Vec::new(),
        })
        .unwrap();
        session
            .add_effect(Chain::Track(0), Path::new("synth.clap"), half)
            .unwrap();
        session
            .add_effect(
                Chain::Master,
                Path::new("synth.clap"),
                clap(refrain_test_synth::EFFECT_ID),
            )
            .unwrap();
        let dir = std::env::temp_dir().join(format!("refrain-session-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("song.refrain");
        session.save(&file).unwrap();
        let saved = Project::read(&file).unwrap();
        std::fs::remove_dir_all(&dir).unwrap();
        let state = |p: &Stored| p.state.data.clone();
        assert_eq!(
            state(&saved.tracks[0].instrument),
            refrain_test_tone::save_state(1.0)
        );
        assert_eq!(state(&saved.tracks[0].effects[0]), effect::save_state(0.5));
        assert_eq!(
            state(&saved.master.effects[0]),
            effect::save_state(effect::DEFAULT_GAIN)
        );
    }
}
