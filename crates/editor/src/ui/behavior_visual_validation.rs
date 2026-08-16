//! Deterministic Behavior Tree presentation used only by desktop visual validation.

use super::*;

/// Returns whether the trusted visual-validation process explicitly requested the
/// Behavior Tree scenario.
#[cfg(feature = "visual-validation")]
pub(super) fn behavior_tree_visual_scenario_requested() -> bool {
    matches!(
        std::env::var("GAMEENGINE_VISUAL_EDITOR_SCENARIO").as_deref(),
        Ok("behavior-tree")
    )
}

/// Draws the schema-driven Behavior Tree Add Node catalog beside the live-debug
/// fixture so one screenshot can verify search and semantic grouping.
#[cfg(feature = "visual-validation")]
pub(super) fn show_behavior_tree_visual_palette(
    context: &egui::Context,
    session: &EditorSession,
) {
    let search = "behavior";
    let kinds = session.available_graph_node_kinds();
    egui::Window::new("Behavior Tree Add Node Palette")
        .id(egui::Id::new("behavior_tree_visual_palette"))
        .fixed_pos(egui::pos2(18.0, 108.0))
        .fixed_size(egui::vec2(285.0, 520.0))
        .resizable(false)
        .collapsible(false)
        .show(context, |ui| {
            ui.label("Schema-driven node search");
            let mut visible_search = search.to_owned();
            ui.add_enabled(
                false,
                egui::TextEdit::singleline(&mut visible_search)
                    .hint_text("Search nodes...")
                    .desired_width(245.0),
            );
            ui.separator();

            let mut last_category = String::new();
            for kind in kinds
                .into_iter()
                .filter(|kind| kind.matches_search(search))
            {
                if kind.category() != last_category.as_str() {
                    if !last_category.is_empty() {
                        ui.separator();
                    }
                    last_category = kind.category().to_owned();
                    ui.strong(&last_category);
                }
                let _ = ui.button(kind.label());
            }
        });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn behavior_search_keeps_schema_categories_visible() {
        let session = EditorSession::behavior_tree_example().expect("reference tree");
        let matching = session
            .available_graph_node_kinds()
            .into_iter()
            .filter(|kind| kind.matches_search("behavior"))
            .collect::<Vec<_>>();

        assert!(!matching.is_empty());
        assert!(matching
            .iter()
            .any(|kind| kind.category() == "Behavior Tree/Composite"));
        assert!(matching
            .iter()
            .any(|kind| kind.category() == "Behavior Tree/Decorator"));
        assert!(matching
            .iter()
            .any(|kind| kind.category() == "Behavior Tree/Condition"));
        assert!(matching
            .iter()
            .any(|kind| kind.category() == "Behavior Tree/Action"));
    }
}
