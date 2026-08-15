//! System-font setup for the Launcher window.
//!
//! The Launcher shows project names and full filesystem paths, which are
//! frequently Japanese on Windows, so the same CJK-first fallback order the
//! Editor installs in `crates/editor/src/editor_fonts.rs` is applied here. The
//! Launcher cannot reuse that function because depending on the Editor crate
//! would pull the renderer into a process that never draws a scene.

use eframe::egui;
use std::path::PathBuf;
use std::sync::Arc;

/// Installs the first available system CJK font and symbol font ahead of
/// egui's compact built-in fonts.
///
/// Missing fonts are skipped, so a system without any of the candidates keeps
/// the built-in fonts instead of failing to start.
pub(crate) fn install_launcher_fonts(context: &egui::Context) {
    let mut definitions = egui::FontDefinitions::default();
    let mut installed_names = Vec::new();

    for (name, candidates) in launcher_font_candidates() {
        let Some(bytes) = candidates.iter().find_map(|path| std::fs::read(path).ok()) else {
            continue;
        };
        definitions
            .font_data
            .insert(name.to_owned(), Arc::new(egui::FontData::from_owned(bytes)));
        installed_names.push(name.to_owned());
    }

    if installed_names.is_empty() {
        return;
    }

    for family in [egui::FontFamily::Proportional, egui::FontFamily::Monospace] {
        if let Some(fonts) = definitions.families.get_mut(&family) {
            fonts.splice(0..0, installed_names.iter().cloned());
        }
    }
    context.set_fonts(definitions);
}

#[cfg(target_os = "windows")]
fn launcher_font_candidates() -> Vec<(&'static str, Vec<PathBuf>)> {
    let windows = std::env::var_os("WINDIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(r"C:\Windows"));
    let fonts = windows.join("Fonts");
    vec![
        (
            "launcher_cjk",
            vec![
                fonts.join("NotoSansJP-VF.ttf"),
                fonts.join("meiryo.ttc"),
                fonts.join("YuGothM.ttc"),
                fonts.join("msgothic.ttc"),
            ],
        ),
        (
            "launcher_symbols",
            vec![fonts.join("seguisym.ttf"), fonts.join("seguiemj.ttf")],
        ),
    ]
}

#[cfg(target_os = "linux")]
fn launcher_font_candidates() -> Vec<(&'static str, Vec<PathBuf>)> {
    vec![
        (
            "launcher_cjk",
            [
                "/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc",
                "/usr/share/fonts/opentype/noto/NotoSansCJKjp-Regular.otf",
                "/usr/share/fonts/truetype/noto/NotoSansCJK-Regular.ttc",
            ]
            .into_iter()
            .map(PathBuf::from)
            .collect(),
        ),
        (
            "launcher_symbols",
            vec![PathBuf::from(
                "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf",
            )],
        ),
    ]
}

#[cfg(target_os = "macos")]
fn launcher_font_candidates() -> Vec<(&'static str, Vec<PathBuf>)> {
    vec![
        (
            "launcher_cjk",
            [
                "/System/Library/Fonts/Hiragino Sans GB.ttc",
                "/System/Library/Fonts/AppleSDGothicNeo.ttc",
            ]
            .into_iter()
            .map(PathBuf::from)
            .collect(),
        ),
        (
            "launcher_symbols",
            vec![PathBuf::from("/System/Library/Fonts/Apple Symbols.ttf")],
        ),
    ]
}

#[cfg(not(any(target_os = "windows", target_os = "linux", target_os = "macos")))]
fn launcher_font_candidates() -> Vec<(&'static str, Vec<PathBuf>)> {
    Vec::new()
}

#[cfg(test)]
mod tests {
    #[cfg(target_os = "windows")]
    use super::*;

    /// Recent-project rows show project names and paths verbatim, so a Windows
    /// Launcher must render Japanese folder names and the status glyphs used by
    /// the row controls without fallback boxes.
    #[cfg(target_os = "windows")]
    #[test]
    fn installed_launcher_fonts_cover_japanese_and_row_glyphs() {
        let context = egui::Context::default();
        install_launcher_fonts(&context);
        context.begin_pass(egui::RawInput::default());
        let font_id = egui::FontId::proportional(14.0);

        assert!(context.fonts_mut(|fonts| fonts.has_glyph(&font_id, '日')));
        assert!(context.fonts_mut(|fonts| fonts.has_glyph(&font_id, '×')));
        assert!(context.fonts_mut(|fonts| fonts.has_glyph(&font_id, '●')));

        let _ = context.end_pass();
    }
}
