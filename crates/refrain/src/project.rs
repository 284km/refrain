//! Commands that change a project file: its tracks, their effects, and the
//! mix. They load a plugin only to learn what it is, and leave it at its own
//! starting state.

use std::path::Path;

use refrain_project::{Format, Plugin as Stored, Project, State, Track};

use crate::plugin::Kind;
use crate::{Options, Result, index_from_one};

pub fn new(path: &Path, o: &Options) -> Result<()> {
    if path.exists() {
        return Err(format!("{} already exists", path.display()).into());
    }
    let project = Project {
        tempo: o.tempo.unwrap_or(120.0),
        ..Project::default()
    };
    project.write(path)?;
    println!("made {}", path.display());
    Ok(())
}

fn describe(stored: &Stored) -> String {
    let format = match stored.format {
        Format::Clap => "CLAP",
        Format::Vst3 => "VST3",
    };
    format!("{format}: {} ({}), {}", stored.name, stored.id, stored.path)
}

fn decibels(db: f32) -> String {
    format!("{db:+.1} dB").replace("+0.0", "0")
}

pub fn show(path: &Path) -> Result<()> {
    let project = Project::read(path)?;
    println!(
        "{}: {} beats per minute, {} tracks",
        path.display(),
        project.tempo,
        project.tracks.len()
    );
    for (t, track) in project.tracks.iter().enumerate() {
        let mix = track.mix;
        let mut notes = vec![
            format!("volume {}", decibels(mix.volume_db)),
            format!("pan {}", mix.pan),
        ];
        if mix.mute {
            notes.push("muted".into());
        }
        if mix.solo {
            notes.push("soloed".into());
        }
        println!("track {}: {} ({})", t + 1, track.name, notes.join(", "));
        println!("  instrument: {}", describe(&track.instrument));
        for (e, effect) in track.effects.iter().enumerate() {
            println!("  effect {}: {}", e + 1, describe(effect));
        }
        let events: usize = track.clips.iter().map(|c| c.events.len()).sum();
        let clips = track.clips.len();
        let plural = |n: usize| if n == 1 { "" } else { "s" };
        println!(
            "  {clips} clip{}, {events} event{}",
            plural(clips),
            plural(events)
        );
    }
    println!("master: volume {}", decibels(project.master.volume_db));
    for (e, effect) in project.master.effects.iter().enumerate() {
        println!("  effect {}: {}", e + 1, describe(effect));
    }
    Ok(())
}

/// Names the plugin at `plugin` for a project at `project`, by loading it.
fn stored_plugin(project: &Path, plugin: &Path, id: Option<&str>) -> Result<Stored> {
    // SAFETY: the user asked for this file to be loaded.
    let kind = unsafe { Kind::open(plugin, id) }?;
    println!("{}", kind.describe());
    let (format, id, name) = kind.identity();
    Ok(Stored::new(
        project,
        format,
        plugin,
        id,
        name,
        State::default(),
    ))
}

pub fn add_track(path: &Path, plugin: &Path, o: &Options) -> Result<()> {
    let mut project = Project::read(path)?;
    let is_project = plugin
        .extension()
        .is_some_and(|e| e.eq_ignore_ascii_case(refrain_project::EXTENSION));
    if is_project {
        if o.name.is_some() || o.id.is_some() {
            return Err("--name and --id are for a track added from a plugin".into());
        }
        let mut other = Project::read(plugin)?;
        other.move_to(plugin, path);
        for track in other.tracks {
            project.tracks.push(track);
            let t = project.tracks.len();
            println!("added track {t}: {}", project.tracks[t - 1].name);
        }
    } else {
        let mut track = Track::new(stored_plugin(path, plugin, o.id.as_deref())?);
        if let Some(name) = &o.name {
            track.name = name.clone();
        }
        project.tracks.push(track);
        println!("added track {}", project.tracks.len());
    }
    project.write(path)?;
    Ok(())
}

pub fn remove_track(path: &Path, n: &str) -> Result<()> {
    let mut project = Project::read(path)?;
    let t = index_from_one(n, "a track")?;
    if t >= project.tracks.len() {
        return Err(format!(
            "there is no track {n}; the project has {}",
            project.tracks.len()
        )
        .into());
    }
    let track = project.tracks.remove(t);
    project.write(path)?;
    println!("removed track {n}: {}", track.name);
    Ok(())
}

/// The effects of the track or the master that `o` chooses.
fn effects<'a>(project: &'a mut Project, o: &Options) -> Result<(&'a mut Vec<Stored>, String)> {
    if o.master {
        return Ok((&mut project.master.effects, "the master".into()));
    }
    let t = o.track.unwrap_or(0);
    let count = project.tracks.len();
    let track = project
        .tracks
        .get_mut(t)
        .ok_or_else(|| format!("there is no track {}; the project has {count}", t + 1))?;
    Ok((&mut track.effects, format!("track {}", t + 1)))
}

pub fn add_effect(path: &Path, plugin: &Path, o: &Options) -> Result<()> {
    let mut project = Project::read(path)?;
    let (chain, owner) = effects(&mut project, o)?;
    chain.push(stored_plugin(path, plugin, o.id.as_deref())?);
    let k = chain.len();
    project.write(path)?;
    println!("added effect {k} of {owner}");
    Ok(())
}

pub fn remove_effect(path: &Path, k: &str, o: &Options) -> Result<()> {
    let mut project = Project::read(path)?;
    let e = index_from_one(k, "an effect")?;
    let (chain, owner) = effects(&mut project, o)?;
    if e >= chain.len() {
        return Err(format!("{owner} has no effect {k}; it has {}", chain.len()).into());
    }
    let effect = chain.remove(e);
    project.write(path)?;
    println!("removed effect {k} of {owner}: {}", effect.name);
    Ok(())
}

pub fn mix(path: &Path, o: &Options) -> Result<()> {
    let mut project = Project::read(path)?;
    if o.volume.is_none() && o.pan.is_none() && o.mute.is_none() && o.solo.is_none() {
        return Err("mix needs something to change: --volume, --pan, --mute or --solo".into());
    }
    if o.master {
        if o.pan.is_some() || o.mute.is_some() || o.solo.is_some() {
            return Err("the master has a volume only".into());
        }
        if let Some(db) = o.volume {
            project.master.volume_db = db;
        }
        project.write(path)?;
        println!("master: volume {}", decibels(project.master.volume_db));
        return Ok(());
    }
    let t = o.track.unwrap_or(0);
    let count = project.tracks.len();
    let track = project
        .tracks
        .get_mut(t)
        .ok_or_else(|| format!("there is no track {}; the project has {count}", t + 1))?;
    let mix = &mut track.mix;
    if let Some(db) = o.volume {
        mix.volume_db = db;
    }
    if let Some(pan) = o.pan {
        mix.pan = pan;
    }
    if let Some(mute) = o.mute {
        mix.mute = mute;
    }
    if let Some(solo) = o.solo {
        mix.solo = solo;
    }
    let mix = *mix;
    project.write(path)?;
    println!(
        "track {}: volume {}, pan {}{}{}",
        t + 1,
        decibels(mix.volume_db),
        mix.pan,
        if mix.mute { ", muted" } else { "" },
        if mix.solo { ", soloed" } else { "" }
    );
    Ok(())
}
