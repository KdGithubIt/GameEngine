//! Modeless authoring windows embedded in the main Engine Editor process.

use crate::authoring_tools::AuthoringTool;
use eframe::{egui, Frame};

/// Owns the state and visibility of every project authoring window.
///
/// The windows share the main editor process and egui context. Opening a tool
/// therefore never starts Cargo or a sibling executable.
#[derive(Default)]
pub struct AuthoringWindows {
    ability_open: bool,
    runtime_event_open: bool,
    ui_contract_open: bool,
    advanced_geometry_open: bool,
    ability: ability::EmbeddedWindow,
    runtime_event: runtime_event::EmbeddedWindow,
    ui_contract: ui_contract::EmbeddedWindow,
    advanced_geometry: advanced_geometry::EmbeddedWindow,
}


impl AuthoringWindows {
    /// Makes the selected authoring window visible.
    pub fn open(&mut self, tool: AuthoringTool) {
        match tool {
            AuthoringTool::AbilityDesigner => self.ability_open = true,
            AuthoringTool::RuntimeEventTimeline => self.runtime_event_open = true,
            AuthoringTool::UiContractDesigner => self.ui_contract_open = true,
            AuthoringTool::AdvancedGeometryDesigner => self.advanced_geometry_open = true,
        }
    }

    /// Draws every visible authoring window into the current editor frame.
    pub fn show(&mut self, context: &egui::Context, frame: &mut Frame) {
        self.ability
            .show(context, frame, &mut self.ability_open);
        self.runtime_event
            .show(context, frame, &mut self.runtime_event_open);
        self.ui_contract
            .show(context, frame, &mut self.ui_contract_open);
        self.advanced_geometry
            .show(context, frame, &mut self.advanced_geometry_open);
    }
}

// The former standalone source remains the single Ability Designer implementation.
#[allow(dead_code)]
mod ability {
    use crate as engine_editor;

    include!("bin/ability_designer.rs");

    #[derive(Default)]
    pub(super) struct EmbeddedWindow {
        state: AbilityDesigner,
    }

    impl EmbeddedWindow {
        pub(super) fn show(
            &mut self,
            context: &egui::Context,
            frame: &mut eframe::Frame,
            open: &mut bool,
        ) {
            egui::Window::new("Ability Designer")
                .id(egui::Id::new("embedded_ability_designer"))
                .open(open)
                .default_width(1_100.0)
                .default_height(760.0)
                .resizable(true)
                .show(context, |ui| {
                    <AbilityDesigner as eframe::App>::ui(&mut self.state, ui, frame);
                });
        }
    }
}

// The former standalone source remains the single event trace implementation.
#[allow(dead_code)]
mod runtime_event {
    use crate as engine_editor;

    include!("bin/runtime_event_viewer.rs");

    #[derive(Default)]
    pub(super) struct EmbeddedWindow {
        state: RuntimeEventViewer,
    }

    impl EmbeddedWindow {
        pub(super) fn show(
            &mut self,
            context: &egui::Context,
            frame: &mut eframe::Frame,
            open: &mut bool,
        ) {
            <RuntimeEventViewer as eframe::App>::logic(&mut self.state, context, frame);
            egui::Window::new("Runtime Event Timeline")
                .id(egui::Id::new("embedded_runtime_event_timeline"))
                .open(open)
                .default_width(1_260.0)
                .default_height(760.0)
                .resizable(true)
                .show(context, |ui| {
                    <RuntimeEventViewer as eframe::App>::ui(&mut self.state, ui, frame);
                });
        }
    }
}

// The former standalone source remains the single UI Contract implementation.
#[allow(dead_code)]
mod ui_contract {
    use crate as engine_editor;

    include!("bin/ui_contract_designer.rs");

    #[derive(Default)]
    pub(super) struct EmbeddedWindow {
        state: UiContractDesigner,
    }

    impl EmbeddedWindow {
        pub(super) fn show(
            &mut self,
            context: &egui::Context,
            frame: &mut eframe::Frame,
            open: &mut bool,
        ) {
            egui::Window::new("UI Contract Designer")
                .id(egui::Id::new("embedded_ui_contract_designer"))
                .open(open)
                .default_width(1_180.0)
                .default_height(780.0)
                .resizable(true)
                .show(context, |ui| {
                    <UiContractDesigner as eframe::App>::ui(&mut self.state, ui, frame);
                });
        }
    }
}

// The former standalone source remains the single geometry implementation.
#[allow(dead_code)]
mod advanced_geometry {
    use crate as engine_editor;

    include!("bin/advanced_geometry_designer.rs");

    #[derive(Default)]
    pub(super) struct EmbeddedWindow {
        state: AdvancedGeometryDesigner,
    }

    impl EmbeddedWindow {
        pub(super) fn show(
            &mut self,
            context: &egui::Context,
            frame: &mut eframe::Frame,
            open: &mut bool,
        ) {
            egui::Window::new("Advanced Geometry Designer")
                .id(egui::Id::new("embedded_advanced_geometry_designer"))
                .open(open)
                .default_width(1_160.0)
                .default_height(800.0)
                .resizable(true)
                .show(context, |ui| {
                    <AdvancedGeometryDesigner as eframe::App>::ui(&mut self.state, ui, frame);
                });
        }
    }
}
