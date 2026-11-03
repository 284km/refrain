//! Plugins of either format, chosen by file extension.

use std::path::Path;

use refrain_clap::{ClapError, ClapPlugin};
use refrain_engine::Processor;
use refrain_project::{Format, State};
use refrain_vst3::{Vst3Error, Vst3Module, Vst3Plugin, Vst3State};

use crate::Result;

/// The size of an editor: in points on macOS, pixels elsewhere.
pub struct EditorSize {
    pub width: u32,
    pub height: u32,
    pub resizable: bool,
}

/// A loaded plugin of either format.
pub enum Kind {
    Clap(ClapPlugin),
    Vst3(Vst3Plugin),
}

impl Kind {
    /// Gives the plugin a saved state. An empty one leaves the plugin as it
    /// started: a plugin that keeps no state saves none, and a plugin just
    /// added to a project has none yet.
    pub fn load_state(&mut self, state: &State) -> Result<()> {
        match self {
            _ if state.data.is_empty() => {}
            Self::Clap(p) => p.load_state(&state.data)?,
            Self::Vst3(p) => p.load_state(&Vst3State {
                component: state.data.clone(),
                controller: state.controller.clone(),
            })?,
        }
        Ok(())
    }

    /// The plugin's state, to be kept in a project.
    pub fn save_state(&mut self) -> Result<State> {
        Ok(match self {
            Self::Clap(p) => State {
                data: p.save_state()?.unwrap_or_default(),
                controller: Vec::new(),
            },
            Self::Vst3(p) => {
                let Vst3State {
                    component,
                    controller,
                } = p.save_state()?;
                State {
                    data: component,
                    controller,
                }
            }
        })
    }

    /// Whether the plugin renders offline, as when writing a file. Takes
    /// effect when the plugin is next activated. Returns whether the plugin
    /// took the mode; one that does not simply goes on as it is.
    pub fn set_offline(&mut self, offline: bool) -> bool {
        match self {
            Self::Clap(p) => p.set_offline(offline),
            // Every VST3 plugin is set up with a process mode.
            Self::Vst3(p) => {
                p.set_offline(offline);
                true
            }
        }
    }

    /// The plugin's format, ID and name, as a project names it.
    pub fn identity(&self) -> (Format, String, String) {
        match self {
            Self::Clap(p) => (Format::Clap, p.info().id.clone(), p.info().name.clone()),
            Self::Vst3(p) => (Format::Vst3, p.info().id.clone(), p.info().name.clone()),
        }
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
