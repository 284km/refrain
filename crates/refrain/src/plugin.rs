//! Plugins of either format, chosen by file extension, and projects that
//! hold one with its state.

use std::ops::{Deref, DerefMut};
use std::path::{Path, PathBuf};

use refrain_clap::{ClapError, ClapPlugin};
use refrain_engine::{Event, Processor, Tempo};
use refrain_project::{Format, Project, State};
use refrain_vst3::{Vst3Error, Vst3Module, Vst3Plugin, Vst3State};

use crate::Result;

/// The size of an editor: in points on macOS, pixels elsewhere.
pub struct EditorSize {
    pub width: u32,
    pub height: u32,
    pub resizable: bool,
}

/// A plugin, the file it came from, and the project it came with, if any.
pub struct Plugin {
    kind: Kind,
    file: PathBuf,
    project: Option<Project>,
}

impl Deref for Plugin {
    type Target = Kind;

    fn deref(&self) -> &Kind {
        &self.kind
    }
}

impl DerefMut for Plugin {
    fn deref_mut(&mut self) -> &mut Kind {
        &mut self.kind
    }
}

impl Plugin {
    /// Loads a plugin from a `.clap` or `.vst3` file (see [`Kind::open`]), or
    /// the plugin of a project, with its state and clips.
    ///
    /// # Safety
    ///
    /// This runs the plugin's code in this process.
    pub unsafe fn open(path: &Path, id: Option<&str>) -> Result<Self> {
        let is_project = path
            .extension()
            .is_some_and(|e| e.eq_ignore_ascii_case(refrain_project::EXTENSION));
        if !is_project {
            let kind = unsafe { Kind::open(path, id) }?;
            return Ok(Self {
                kind,
                file: path.to_path_buf(),
                project: None,
            });
        }
        let project = Project::read(path)?;
        let file = project.plugin_path(path);
        let instrument = &project.instrument;
        let mut kind = unsafe { Kind::open(&file, Some(&instrument.id)) }.map_err(|e| {
            format!(
                "{}: the project's plugin, {}: {e}",
                path.display(),
                file.display()
            )
        })?;
        kind.load_state(&instrument.state)?;
        let clips = match project.clips.len() {
            0 => String::new(),
            1 => ", 1 clip".to_owned(),
            n => format!(", {n} clips"),
        };
        println!(
            "project {}: {} with its saved state{clips}, at {} beats per minute",
            path.display(),
            instrument.name,
            project.tempo
        );
        Ok(Self {
            kind,
            file,
            project: Some(project),
        })
    }

    pub fn tempo(&self) -> Tempo {
        self.project
            .as_ref()
            .map_or(Tempo::new(120.0), Project::tempo)
    }

    pub fn set_tempo(&mut self, beats_per_minute: f64) {
        self.project_mut().tempo = beats_per_minute;
    }

    /// The clips' events at frames from the start, at `sample_rate`.
    pub fn sequence(&self, sample_rate: f32) -> Vec<(u64, Event)> {
        let Some(project) = &self.project else {
            return Vec::new();
        };
        let tempo = project.tempo();
        project
            .sequence()
            .into_iter()
            .map(|(tick, event)| (tempo.frame_at(tick, sample_rate), event))
            .collect()
    }

    /// The frame just after the clips' last event, at `sample_rate`.
    pub fn sequence_end(&self, sample_rate: f32) -> u64 {
        self.project
            .as_ref()
            .map_or(0, |p| p.tempo().frame_at(p.end(), sample_rate))
    }

    /// Adds a clip at the start of the timeline, holding `events` at ticks.
    pub fn add_clip(&mut self, events: &[(u64, Event)]) {
        self.project_mut().add_clip(0, events);
    }

    /// The project, made empty if the plugin came without one. Its path and
    /// state are filled in when it is saved.
    fn project_mut(&mut self) -> &mut Project {
        let (format, id, name) = match &self.kind {
            Kind::Clap(p) => (Format::Clap, p.info().id.clone(), p.info().name.clone()),
            Kind::Vst3(p) => (Format::Vst3, p.info().id.clone(), p.info().name.clone()),
        };
        let file = self.file.clone();
        self.project.get_or_insert_with(|| {
            Project::new(Path::new("."), format, &file, id, name, State::default())
        })
    }

    /// Writes a project holding this plugin, its current state, and the
    /// clips it came with or recorded.
    pub fn save_project(&mut self, file: &Path) -> Result<()> {
        let state = match &mut self.kind {
            Kind::Clap(p) => State {
                data: p.save_state()?.unwrap_or_default(),
                controller: Vec::new(),
            },
            Kind::Vst3(p) => {
                let Vst3State {
                    component,
                    controller,
                } = p.save_state()?;
                State {
                    data: component,
                    controller,
                }
            }
        };
        let plugin = self.file.clone();
        let project = self.project_mut();
        project.set_instrument(file, &plugin, state);
        project.write(file)?;
        Ok(())
    }
}

pub enum Kind {
    Clap(ClapPlugin),
    Vst3(Vst3Plugin),
}

impl Kind {
    fn load_state(&mut self, state: &State) -> Result<()> {
        match self {
            // A plugin that keeps no state saves none, and is given none.
            Self::Clap(_) if state.data.is_empty() => {}
            Self::Clap(p) => p.load_state(&state.data)?,
            Self::Vst3(p) => p.load_state(&Vst3State {
                component: state.data.clone(),
                controller: state.controller.clone(),
            })?,
        }
        Ok(())
    }

    /// Loads the plugin `id` from a `.clap` or `.vst3` file. For VST3, `id` is
    /// the plugin's name or class ID. Without an id, a file that holds several
    /// plugins gives its first one, and says so.
    ///
    /// # Safety
    ///
    /// This runs the file's code in this process.
    pub unsafe fn open(path: &Path, id: Option<&str>) -> Result<Self> {
        let extension = path
            .extension()
            .map(|e| e.to_string_lossy().to_lowercase())
            .unwrap_or_default();
        let first = |ids: Vec<String>| -> String {
            println!(
                "this file holds {} plugins; using the first. Choose with --id: {}",
                ids.len(),
                ids.join(", ")
            );
            ids.into_iter().next().unwrap_or_default()
        };
        match extension.as_str() {
            "clap" => {
                let entry = unsafe { refrain_clap::load_entry(path) }?;
                let plugin = match ClapPlugin::new(&entry, id) {
                    Err(ClapError::Ambiguous(ids)) => ClapPlugin::new(&entry, Some(&first(ids)))?,
                    other => other?,
                };
                Ok(Self::Clap(plugin))
            }
            "vst3" => {
                let module = unsafe { Vst3Module::load(path) }?;
                let plugin = match Vst3Plugin::new(&module, id) {
                    Err(Vst3Error::Ambiguous(names)) => {
                        Vst3Plugin::new(&module, Some(&first(names)))?
                    }
                    other => other?,
                };
                Ok(Self::Vst3(plugin))
            }
            _ => Err(format!("{} is not a .clap or .vst3 file", path.display()).into()),
        }
    }

    pub fn describe(&self) -> String {
        match self {
            Self::Clap(p) => format!("CLAP: {}", p.info()),
            Self::Vst3(p) => format!("VST3: {}", p.info()),
        }
    }

    pub fn activate(
        &mut self,
        sample_rate: f32,
        max_block_frames: usize,
    ) -> Result<Box<dyn Processor>> {
        Ok(match self {
            Self::Clap(p) => Box::new(p.activate(sample_rate, max_block_frames)?),
            Self::Vst3(p) => Box::new(p.activate(sample_rate, max_block_frames)?),
        })
    }

    /// Opens the plugin's editor inside `parent`. Sizes are in points on
    /// macOS and pixels elsewhere. `None` when there is no editor to embed.
    ///
    /// # Safety
    ///
    /// `parent` must stay valid until the editor is closed.
    pub unsafe fn open_editor(
        &mut self,
        parent: winit::raw_window_handle::RawWindowHandle,
        scale: f64,
    ) -> Result<Option<EditorSize>> {
        Ok(match self {
            Self::Clap(p) => unsafe { p.open_editor(parent, scale) }?.map(|s| EditorSize {
                width: s.width,
                height: s.height,
                resizable: s.resizable,
            }),
            Self::Vst3(p) => unsafe { p.open_editor(parent, scale) }?.map(|s| EditorSize {
                width: s.width,
                height: s.height,
                resizable: s.resizable,
            }),
        })
    }

    pub fn resize_editor(&mut self, width: u32, height: u32) -> (u32, u32) {
        match self {
            Self::Clap(p) => p.resize_editor(width, height),
            Self::Vst3(p) => p.resize_editor(width, height),
        }
    }

    pub fn take_editor_resize_request(&self) -> Option<(u32, u32)> {
        match self {
            Self::Clap(p) => p.take_editor_resize_request(),
            Self::Vst3(p) => p.take_editor_resize_request(),
        }
    }

    /// Whether the plugin closed its own editor.
    pub fn editor_closed(&self) -> bool {
        match self {
            Self::Clap(p) => p.editor_closed(),
            Self::Vst3(_) => false,
        }
    }

    pub fn close_editor(&mut self) {
        match self {
            Self::Clap(p) => p.close_editor(),
            Self::Vst3(p) => p.close_editor(),
        }
    }

    /// Answers the plugin's requests for the main thread.
    pub fn poll_main_thread(&mut self) {
        match self {
            Self::Clap(p) => p.poll_main_thread(),
            // refrain answers VST3 plugins' calls as they come.
            Self::Vst3(_) => {}
        }
    }
}

/// Lists installed CLAP and VST3 plugins.
pub fn list_installed() {
    let formats = [
        (
            "CLAP",
            refrain_clap::standard_paths(),
            refrain_clap::find_clap_files as fn(&_) -> _,
        ),
        (
            "VST3",
            refrain_vst3::standard_paths(),
            refrain_vst3::find_vst3_files,
        ),
    ];
    for (format, dirs, find) in formats {
        println!("{format}");
        for dir in &dirs {
            println!("  searching {}", dir.display());
        }
        let files = find(&dirs);
        if files.is_empty() {
            println!("  none found");
        }
        for file in files {
            println!("  {}", file.display());
            // SAFETY: the user installed these plugins; listing them means
            // running their code, as every host does.
            let listed: Result<Vec<String>> = unsafe {
                match format {
                    "CLAP" => refrain_clap::load_entry(&file)
                        .and_then(|e| refrain_clap::plugins_in(&e))
                        .map(|ps| ps.iter().map(ToString::to_string).collect())
                        .map_err(Into::into),
                    _ => Vst3Module::load(&file)
                        .map(|m| m.plugins().iter().map(ToString::to_string).collect())
                        .map_err(Into::into),
                }
            };
            match listed {
                Ok(plugins) => plugins.iter().for_each(|p| println!("      {p}")),
                Err(e) => println!("      {e}"),
            }
        }
    }
}
