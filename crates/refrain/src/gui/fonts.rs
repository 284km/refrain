//! The GUI's fonts, read from the system.
//!
//! egui's own fonts come under font licenses this repository does not take,
//! so refrain uses fonts every macOS and Windows installation has. Japanese
//! fonts come after the Latin ones, for the characters those lack.

use std::path::PathBuf;
use std::sync::Arc;

use egui::{FontData, FontDefinitions, FontFamily};

/// Font files, as a path and a face index, from the first to try.
fn candidates(family: &FontFamily) -> Vec<(PathBuf, u32)> {
    let mut list = Vec::new();
    #[cfg(target_os = "macos")]
    {
        let fonts = PathBuf::from("/System/Library/Fonts");
        match family {
            FontFamily::Monospace => list.push((fonts.join("Menlo.ttc"), 0)),
            _ => list.push((fonts.join("Helvetica.ttc"), 0)),
        }
        list.push((fonts.join("ヒラギノ角ゴシック W3.ttc"), 0));
    }
    #[cfg(windows)]
    {
        let windows =
            std::env::var_os("WINDIR").map_or_else(|| PathBuf::from("C:\\Windows"), PathBuf::from);
        let fonts = windows.join("Fonts");
        match family {
            FontFamily::Monospace => list.push((fonts.join("consola.ttf"), 0)),
            _ => list.push((fonts.join("segoeui.ttf"), 0)),
        }
        list.push((fonts.join("YuGothM.ttc"), 0));
        list.push((fonts.join("meiryo.ttc"), 0));
    }
    #[cfg(target_os = "linux")]
    {
        let fonts = PathBuf::from("/usr/share/fonts");
        match family {
            FontFamily::Monospace => {
                list.push((fonts.join("truetype/dejavu/DejaVuSansMono.ttf"), 0))
            }
            _ => list.push((fonts.join("truetype/dejavu/DejaVuSans.ttf"), 0)),
        }
        list.push((fonts.join("opentype/noto/NotoSansCJK-Regular.ttc"), 0));
    }
    let _ = family;
    list
}

/// Fonts for the GUI, from the files of the system that are there. `None`
/// when none of them are.
pub fn definitions() -> Option<FontDefinitions> {
    let mut definitions = FontDefinitions::empty();
    for family in [FontFamily::Proportional, FontFamily::Monospace] {
        let mut names = Vec::new();
        for (path, index) in candidates(&family) {
            let name = format!("{}#{index}", path.display());
            if !definitions.font_data.contains_key(&name) {
                let Ok(bytes) = std::fs::read(&path) else {
                    continue;
                };
                let mut data = FontData::from_owned(bytes);
                data.index = index;
                definitions.font_data.insert(name.clone(), Arc::new(data));
            }
            names.push(name);
        }
        if names.is_empty() {
            return None;
        }
        definitions.families.insert(family, names);
    }
    Some(definitions)
}

/// Gives `ctx` the system's fonts. Without any, egui could draw no text, so
/// this fails.
pub fn install(ctx: &egui::Context) -> crate::Result<()> {
    let definitions = definitions().ok_or("none of the system fonts the GUI uses were found")?;
    ctx.set_fonts(definitions);
    Ok(())
}
