use super::EditorApp;
use engine_authoring::{
    replace_file_contents, AssetId, ComponentTypeId, Value, VfxAuthoringService, VfxTemplate,
};
use std::collections::BTreeMap;
use std::path::PathBuf;

impl EditorApp {
    pub fn prepare_vfx_visual_validation(&mut self) {
        if let Err(error) = self.prepare_vfx_visual_validation_fixture() {
            eprintln!("VFX visual validation fixture failed: {error}");
        }
    }

    fn prepare_vfx_visual_validation_fixture(&mut self) -> Result<(), String> {
        let project = self
            .project_root
            .clone()
            .ok_or_else(|| "open a project before preparing VFX visual validation".to_owned())?;
        let service = VfxAuthoringService::new();
        let effect = service.template(VfxTemplate::Smoke);
        let json = service
            .effect_to_canonical_json(&effect)
            .map_err(|error| error.to_string())?;
        let relative_path = PathBuf::from("vfx/visual_validation_smoke.vfx.json");
        let asset_path = project.assets_root().join(&relative_path);
        if let Some(parent) = asset_path.parent() {
            std::fs::create_dir_all(parent).map_err(|error| error.to_string())?;
        }
        replace_file_contents(&asset_path, &json).map_err(|error| error.to_string())?;

        let effect_asset = AssetId::generate();
        self.asset_manifest.insert(
            effect_asset.clone(),
            engine::asset::ManifestEntry {
                path: relative_path,
                name: Some("Visual Validation Smoke".to_owned()),
                import_settings: engine::asset::ImportSettings::default(),
            },
        );

        let component_type = ComponentTypeId::new(engine::scene_bridge::VFX_PLAYER_COMPONENT);
        let registry = engine::builtin_registry();
        let definition = registry
            .get(&component_type)
            .ok_or_else(|| "missing built-in VFX Player schema".to_owned())?;
        let mut component = definition.schema.default_value();
        let Value::Object(fields) = &mut component else {
            return Err("VFX Player schema is not object-valued".to_owned());
        };
        fields.insert("effect".to_owned(), Value::AssetRef(effect_asset));
        fields.insert("looping".to_owned(), Value::Bool(true));
        fields.insert("parameter_overrides".to_owned(), Value::Object(BTreeMap::new()));

        let entity = self
            .session
            .create_scene_entity("visual_vfx_player")
            .map_err(|error| error.to_string())?;
        self.session
            .add_scene_component(entity.clone(), component_type, component)
            .map_err(|error| error.to_string())?;
        self.selected_entity = Some(entity.clone());
        if let Some(scene) = self.session.scene() {
            let _ = self.scene_view.focus_entity(scene, &entity);
        }
        self.scene_view.restart_particle_preview();
        self.scene_view.invalidate_asset_preview();
        Ok(())
    }
}
