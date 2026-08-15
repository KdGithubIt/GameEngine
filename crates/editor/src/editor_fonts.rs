//! Shared system-font setup for editor-owned egui contexts.

use std::path::PathBuf;
use std::sync::Arc;

/// Installs the first available system CJK font and symbol font ahead of
/// egui's compact built-in fonts.
///
/// The editor's root context and offscreen preview contexts call the same
/// function so Japanese UI remains readable after viewport isolation.
pub fn install_editor_fonts(context: &eframe::egui::Context) {
    let mut definitions = eframe::egui::FontDefinitions::default();
    let mut installed_names = Vec::new();

    for (name, candidates) in editor_font_candidates() {
        let Some(bytes) = candidates.iter().find_map(|path| std::fs::read(path).ok()) else {
            continue;
        };
        definitions.font_data.insert(
            name.to_owned(),
            Arc::new(eframe::egui::FontData::from_owned(bytes)),
        );
        installed_names.push(name.to_owned());
    }

    if installed_names.is_empty() {
        return;
    }

    for family in [
        eframe::egui::FontFamily::Proportional,
        eframe::egui::FontFamily::Monospace,
    ] {
        if let Some(fonts) = definitions.families.get_mut(&family) {
            fonts.splice(0..0, installed_names.iter().cloned());
        }
    }
    context.set_fonts(definitions);
}

#[cfg(target_os = "windows")]
fn editor_font_candidates() -> Vec<(&'static str, Vec<PathBuf>)> {
    let windows = std::env::var_os("WINDIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(r"C:\Windows"));
    let fonts = windows.join("Fonts");
    vec![
        (
            "editor_cjk",
            vec![
                fonts.join("NotoSansJP-VF.ttf"),
                fonts.join("meiryo.ttc"),
                fonts.join("YuGothM.ttc"),
                fonts.join("msgothic.ttc"),
            ],
        ),
        (
            "editor_symbols",
            vec![fonts.join("seguisym.ttf"), fonts.join("seguiemj.ttf")],
        ),
    ]
}

#[cfg(target_os = "linux")]
fn editor_font_candidates() -> Vec<(&'static str, Vec<PathBuf>)> {
    vec![
        (
            "editor_cjk",
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
            "editor_symbols",
            vec![PathBuf::from(
                "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf",
            )],
        ),
    ]
}

#[cfg(target_os = "macos")]
fn editor_font_candidates() -> Vec<(&'static str, Vec<PathBuf>)> {
    vec![
        (
            "editor_cjk",
            [
                "/System/Library/Fonts/ヒラギノ角ゴシック W3.ttc",
                "/System/Library/Fonts/AppleSDGothicNeo.ttc",
            ]
            .into_iter()
            .map(PathBuf::from)
            .collect(),
        ),
        (
            "editor_symbols",
            vec![PathBuf::from("/System/Library/Fonts/Apple Symbols.ttf")],
        ),
    ]
}

#[cfg(not(any(target_os = "windows", target_os = "linux", target_os = "macos")))]
fn editor_font_candidates() -> Vec<(&'static str, Vec<PathBuf>)> {
    Vec::new()
}

#[cfg(test)]
mod tests {
    #[cfg(target_os = "windows")]
    use super::*;

    /// Windows editor installations must render both Japanese file names and
    /// the symbols used by tabs and hierarchy controls without fallback boxes.
    #[cfg(target_os = "windows")]
    #[test]
    fn installed_editor_fonts_cover_japanese_and_common_symbols() {
        let context = eframe::egui::Context::default();
        install_editor_fonts(&context);
        context.begin_pass(eframe::egui::RawInput::default());
        let font_id = eframe::egui::FontId::proportional(14.0);

        assert!(context.fonts_mut(|fonts| fonts.has_glyph(&font_id, '日')));
        assert!(context.fonts_mut(|fonts| fonts.has_glyph(&font_id, '●')));
        assert!(context.fonts_mut(|fonts| fonts.has_glyph(&font_id, '×')));
        // Expand/collapse arrows used by the Add Component toggle.
        assert!(context.fonts_mut(|fonts| fonts.has_glyph(&font_id, '⏷')));
        assert!(context.fonts_mut(|fonts| fonts.has_glyph(&font_id, '⏶')));

        let _ = context.end_pass();
    }
}
