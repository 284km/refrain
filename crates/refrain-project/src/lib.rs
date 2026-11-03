//! refrain's project files.
//!
//! A project names an instrument plugin and holds its state, so opening the
//! project brings the sound back. It is JSON, so it can be read and diffed:
//!
//! ```json
//! {
//!   "refrain": 1,
//!   "instrument": {
//!     "format": "vst3",
//!     "path": "plugins/Synth.vst3",
//!     "id": "7E5C0A312B9D4F1A9C3E6D521F0A8B44",
//!     "name": "Synth",
//!     "state": { "data": "cnRzMQAAAD8=", "controller": "" }
//!   }
//! }
//! ```
//!
//! States are base64. `data` is a CLAP plugin's state or a VST3 component's;
//! `controller` is a VST3 controller's, and empty for CLAP. A relative `path`
//! is relative to the project file's folder, so a project can be moved
//! together with the plugins next to it.

use std::error::Error;
use std::fmt;
use std::path::{Component, Path, PathBuf};

use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64;
use serde::{Deserialize, Serialize};

/// The file extension of projects.
pub const EXTENSION: &str = "refrain";

/// The version of the format this crate reads and writes.
pub const VERSION: u32 = 1;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Project {
    /// The format version.
    pub refrain: u32,
    pub instrument: Instrument,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Format {
    Clap,
    Vst3,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Instrument {
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
}

impl fmt::Display for ProjectError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(path, e) => write!(f, "{}: {e}", path.display()),
            Self::Parse(path, e) => write!(f, "{} is not a refrain project: {e}", path.display()),
            Self::Version(v) => write!(
                f,
                "the project is version {v}; this refrain reads version {VERSION}"
            ),
        }
    }
}

impl Error for ProjectError {}

impl Project {
    /// A project for the plugin at `plugin` (relative to the current folder,
    /// or absolute), to be written to `file`.
    pub fn new(
        file: &Path,
        format: Format,
        plugin: &Path,
        id: String,
        name: String,
        state: State,
    ) -> Self {
        Self {
            refrain: VERSION,
            instrument: Instrument {
                format,
                path: store_path(plugin, file),
                id,
                name,
                state,
            },
        }
    }

    pub fn read(file: &Path) -> Result<Self, ProjectError> {
        let text = std::fs::read_to_string(file).map_err(|e| ProjectError::Io(file.into(), e))?;
        let project: Project =
            serde_json::from_str(&text).map_err(|e| ProjectError::Parse(file.into(), e))?;
        if project.refrain > VERSION {
            return Err(ProjectError::Version(project.refrain));
        }
        Ok(project)
    }

    /// Writes the project to `file`, replacing it only once the new contents
    /// are complete.
    pub fn write(&self, file: &Path) -> Result<(), ProjectError> {
        let mut text = serde_json::to_string_pretty(self).expect("projects serialize");
        text.push('\n');
        let partial = file.with_extension("refrain-partial");
        std::fs::write(&partial, text).map_err(|e| ProjectError::Io(partial.clone(), e))?;
        std::fs::rename(&partial, file).map_err(|e| ProjectError::Io(file.into(), e))
    }

    /// Where the plugin file is, for a project read from `file`.
    pub fn plugin_path(&self, file: &Path) -> PathBuf {
        let stored = PathBuf::from(&self.instrument.path);
        if stored.is_absolute() {
            stored
        } else {
            fold_parents(&folder_of(file).join(stored))
        }
    }
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

    fn sample() -> Project {
        Project {
            refrain: VERSION,
            instrument: Instrument {
                format: Format::Vst3,
                path: "plugins/Synth.vst3".into(),
                id: "7E5C0A312B9D4F1A9C3E6D521F0A8B44".into(),
                name: "Synth".into(),
                state: State {
                    data: b"rts1\0\0\0\x3f".to_vec(),
                    controller: Vec::new(),
                },
            },
        }
    }

    #[test]
    fn reads_what_the_documentation_shows() {
        let text = r#"{
          "refrain": 1,
          "instrument": {
            "format": "vst3",
            "path": "plugins/Synth.vst3",
            "id": "7E5C0A312B9D4F1A9C3E6D521F0A8B44",
            "name": "Synth",
            "state": { "data": "cnRzMQAAAD8=", "controller": "" }
          }
        }"#;
        let project: Project = serde_json::from_str(text).unwrap();
        assert_eq!(project, sample());
    }

    #[test]
    fn writes_and_reads_back() {
        let dir = std::env::temp_dir().join(format!("refrain-project-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("song.refrain");
        sample().write(&file).unwrap();
        assert_eq!(Project::read(&file).unwrap(), sample());
        assert!(!file.with_extension("refrain-partial").exists());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_missing_controller_state_reads_as_empty() {
        let text = r#"{"refrain": 1, "instrument": {"format": "clap", "path": "a.clap",
            "id": "x", "name": "x", "state": {"data": ""}}}"#;
        let project: Project = serde_json::from_str(text).unwrap();
        assert!(project.instrument.state.controller.is_empty());
    }

    #[test]
    fn refuses_newer_versions_and_bad_base64() {
        let dir = std::env::temp_dir().join(format!("refrain-project-v-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("new.refrain");
        let mut newer = sample();
        newer.refrain = VERSION + 1;
        newer.write(&file).unwrap();
        assert!(matches!(
            Project::read(&file),
            Err(ProjectError::Version(_))
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
        let project = Project {
            instrument: Instrument {
                path: stored,
                ..sample().instrument
            },
            ..sample()
        };
        let resolved = project.plugin_path(Path::new("songs/a.refrain"));
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
    fn absolute_plugin_paths_stay_absolute() {
        let plugin = absolute(Path::new("Synth.vst3"));
        let stored = store_path(&plugin, Path::new("songs/a.refrain"));
        let project = Project {
            instrument: Instrument {
                path: stored,
                ..sample().instrument
            },
            ..sample()
        };
        assert_eq!(project.plugin_path(Path::new("songs/a.refrain")), plugin);
    }
}
