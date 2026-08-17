//! Transient spatial-audio authoring helpers for the Inspector.

use super::*;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(super) enum AuditionListenerSource {
    #[default]
    GameListener,
    SceneView,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum AudioInspectorAction {
    Play,
    Stop,
    Restart,
    UseGameListener,
    UseSceneViewListener,
}

pub(super) struct AudioAuditionState {
    audio: Option<engine::audio::AudioSystem>,
    assets: engine::asset::Assets<engine::audio::AudioAsset>,
    asset_server: Option<engine::asset::AssetServer>,
    asset_root: Option<PathBuf>,
    voice: Option<engine::audio::AudioVoiceId>,
    active_entity: Option<EntityId>,
    pub(super) listener_source: AuditionListenerSource,
    status: String,
}

impl Default for AudioAuditionState {
    fn default() -> Self {
        Self {
            audio: None,
            assets: engine::asset::Assets::default(),
            asset_server: None,
            asset_root: None,
            voice: None,
            active_entity: None,
            listener_source: AuditionListenerSource::GameListener,
            status: "Stopped".to_owned(),
        }
    }
}

impl AudioAuditionState {
    pub(super) fn poll(&mut self) {
        let Some(audio) = self.audio.as_mut() else { return; };
        let completed = audio.drain_completed_voices();
        if self.voice.is_some_and(|voice| completed.contains(&voice)) {
            self.voice = None;
            self.active_entity = None;
            self.status = "Audition finished".to_owned();
        }
    }

    pub(super) fn reset_project(&mut self) {
        self.stop();
        self.assets = engine::asset::Assets::default();
        self.asset_server = None;
        self.asset_root = None;
        self.listener_source = AuditionListenerSource::GameListener;
        self.status = "Stopped".to_owned();
    }

    fn stop(&mut self) {
        self.active_entity = None;
        if let (Some(audio), Some(voice)) = (self.audio.as_mut(), self.voice.take())
            && let Err(error) = audio.stop_voice(voice)
        {
            self.status = format!("Stop failed: {error}");
            return;
        }
        self.status = "Stopped".to_owned();
    }

    fn play(
        &mut self,
        entity: EntityId,
        asset_root: &Path,
        asset_id: AssetId,
        asset_path: &str,
        gains: engine::audio::StereoGains,
        looping: bool,
    ) {
        self.stop();
        if self.asset_root.as_deref() != Some(asset_root) {
            self.assets = engine::asset::Assets::default();
            self.asset_server = Some(engine::asset::AssetServer::with_assets_root(asset_root));
            self.asset_root = Some(asset_root.to_path_buf());
        }
        if self.audio.is_none() {
            match engine::audio::AudioSystem::new() {
                Ok(audio) => self.audio = Some(audio),
                Err(error) => {
                    self.status = format!("Audio device unavailable: {error}");
                    return;
                }
            }
        }
        let Some(server) = self.asset_server.as_mut() else { return; };
        let handle = match server.load_audio(asset_id, asset_path, &mut self.assets) {
            Ok(handle) => handle,
            Err(error) => {
                self.status = format!("Audition load failed: {error}");
                return;
            }
        };
        let Some(asset) = self.assets.get(&handle) else {
            self.status = "Audition clip disappeared from the transient cache".to_owned();
            return;
        };
        match self.audio.as_mut().expect("audio initialized above").start_voice(asset, gains, looping) {
            Ok(voice) => {
                self.voice = Some(voice);
                self.active_entity = Some(entity);
                self.status = "Playing transient audition".to_owned();
            }
            Err(error) => self.status = format!("Audition failed: {error}"),
        }
    }

    fn active_entity(&self) -> Option<EntityId> {
        self.active_entity.clone()
    }

    fn update_gains(&mut self, gains: engine::audio::StereoGains) {
        let (Some(audio), Some(voice)) = (self.audio.as_ref(), self.voice) else { return; };
        if let Err(error) = audio.update_voice(voice, gains) {
            self.status = format!("Audition update failed: {error}");
        }
    }
}

#[derive(Clone)]
struct EmitterPreview {
    clip: AssetId,
    settings: engine::audio::AudioVoiceSpatialSettings,
    looping: bool,
}

pub(super) fn show_audio_component_extras(
    ui: &mut egui::Ui,
    component_type: &ComponentTypeId,
    value: &Value,
    audition: &AudioAuditionState,
) -> Option<AudioInspectorAction> {
    if component_type.as_str() == engine::scene_bridge::AUDIO_EMITTER_COMPONENT {
        ui.separator();
        ui.strong("Attenuation Preview");
        if let Some(emitter) = parse_emitter(value) {
            draw_attenuation_preview(ui, emitter.settings);
        } else {
            ui.weak("Complete the emitter fields to preview attenuation.");
        }
        ui.separator();
        ui.strong("Audition");
        let mut action = None;
        control_row(ui, |ui| {
            ui.label("Listener");
            if ui.selectable_label(audition.listener_source == AuditionListenerSource::GameListener, "Game Listener").clicked() {
                action = Some(AudioInspectorAction::UseGameListener);
            }
            if ui.selectable_label(audition.listener_source == AuditionListenerSource::SceneView, "Scene View").clicked() {
                action = Some(AudioInspectorAction::UseSceneViewListener);
            }
        });
        control_row(ui, |ui| {
            if ui.button("▶ Play").clicked() { action = Some(AudioInspectorAction::Play); }
            if ui.button("■ Stop").clicked() { action = Some(AudioInspectorAction::Stop); }
            if ui.button("↻ Restart").clicked() { action = Some(AudioInspectorAction::Restart); }
            ui.weak(&audition.status);
        });
        ui.small("Audition is transient and uses the production spatial evaluator and managed voice backend.");
        action
    } else if component_type.as_str() == engine::scene_bridge::AUDIO_LISTENER_COMPONENT {
        ui.separator();
        ui.small("Scene View shows this listener's world-space orientation. Highest enabled priority wins; equal highest priorities use deterministic entity order.");
        None
    } else {
        None
    }
}

impl EditorApp {
    pub(super) fn update_audio_audition(&mut self) {
        let Some(active_entity) = self.audio_audition.active_entity() else { return; };
        let snapshot = self.session.scene().and_then(|scene| {
            let component = ComponentTypeId::new(engine::scene_bridge::AUDIO_EMITTER_COMPONENT);
            let value = scene.entity(&active_entity)?.components.get(&component)?;
            let emitter = parse_emitter(value)?;
            let emitter_pose = SceneView::authoring_audio_emitter_pose(scene, &active_entity)?;
            let game_listener = active_game_listener(scene)
                .and_then(|id| SceneView::authoring_audio_listener_pose(scene, &id));
            Some((emitter, emitter_pose, game_listener))
        });
        let Some((emitter, emitter_pose, game_listener)) = snapshot else {
            self.audio_audition.stop();
            self.audio_audition.status = "Audition stopped because the emitter is no longer available".to_owned();
            return;
        };
        let listener = match self.audio_audition.listener_source {
            AuditionListenerSource::GameListener => game_listener,
            AuditionListenerSource::SceneView => Some(self.scene_view.editor_audio_listener_pose()),
        };
        self.audio_audition.update_gains(audition_spatial_gains(listener, emitter_pose, emitter.settings));
    }

    pub(super) fn handle_audio_inspector_action(
        &mut self,
        selected: &EntityId,
        value: &Value,
        action: AudioInspectorAction,
    ) {
        match action {
            AudioInspectorAction::Stop => { self.audio_audition.stop(); return; }
            AudioInspectorAction::UseGameListener => { self.audio_audition.listener_source = AuditionListenerSource::GameListener; return; }
            AudioInspectorAction::UseSceneViewListener => { self.audio_audition.listener_source = AuditionListenerSource::SceneView; return; }
            AudioInspectorAction::Play | AudioInspectorAction::Restart => {}
        }
        let Some(emitter) = parse_emitter(value) else { self.audio_audition.status = "Emitter fields are incomplete".to_owned(); return; };
        let Some(scene) = self.session.scene() else { self.audio_audition.status = "Open a scene before auditioning audio".to_owned(); return; };
        let Some(emitter_pose) = SceneView::authoring_audio_emitter_pose(scene, selected) else { self.audio_audition.status = "The emitter has no resolved world transform".to_owned(); return; };
        let listener = match self.audio_audition.listener_source {
            AuditionListenerSource::SceneView => Some(self.scene_view.editor_audio_listener_pose()),
            AuditionListenerSource::GameListener => active_game_listener(scene).and_then(|id| SceneView::authoring_audio_listener_pose(scene, &id)),
        };
        let gains = audition_spatial_gains(listener, emitter_pose, emitter.settings);
        let Some(project) = self.project_root.as_ref() else { self.audio_audition.status = "Open a project before auditioning audio".to_owned(); return; };
        let Some(entry) = self.asset_manifest.get(&emitter.clip) else { self.audio_audition.status = "Emitter clip is not registered in the asset manifest".to_owned(); return; };
        self.audio_audition.play(selected.clone(), &project.assets_root(), emitter.clip, &entry.path, gains, emitter.looping);
    }

    #[cfg(feature = "visual-validation")]
    pub fn prepare_spatial_audio_visual_validation(&mut self) {
        let registry = engine::components::builtin_registry();
        let transform_type = ComponentTypeId::new(engine::scene_bridge::TRANSFORM_COMPONENT);
        let emitter_type = ComponentTypeId::new(engine::scene_bridge::AUDIO_EMITTER_COMPONENT);
        let listener_type = ComponentTypeId::new(engine::scene_bridge::AUDIO_LISTENER_COMPONENT);
        let Some(transform) = registry.get(&transform_type).map(|definition| definition.schema.default_value()) else { return; };
        let Some(mut emitter) = registry.get(&emitter_type).map(|definition| definition.schema.default_value()) else { return; };
        let Some(listener) = registry.get(&listener_type).map(|definition| definition.schema.default_value()) else { return; };
        let Value::Object(fields) = &mut emitter else { return; };
        fields.insert("clip".to_owned(), Value::AssetRef(AssetId::generate()));
        fields.insert("spatial_blend".to_owned(), Value::F64(1.0));
        fields.insert("min_distance".to_owned(), Value::F64(1.5));
        fields.insert("max_distance".to_owned(), Value::F64(6.0));
        fields.insert("rolloff".to_owned(), Value::String("linear".to_owned()));
        fields.insert("looping".to_owned(), Value::Bool(true));
        let Ok(entity) = self.session.create_scene_entity("Spatial Audio Preview") else { return; };
        if self.session.add_scene_component(entity.clone(), transform_type, transform).is_err()
            || self.session.add_scene_component(entity.clone(), emitter_type, emitter).is_err()
            || self.session.add_scene_component(entity.clone(), listener_type, listener).is_err()
        { return; }
        self.select_single_entity(Some(entity.clone()));
        if let Some(scene) = self.session.scene() {
            let _ = self.scene_view.focus_entity(scene, &entity);
        }
    }
}

fn audition_spatial_gains(
    listener: Option<engine::audio::AudioListenerPose>,
    emitter: engine::audio::AudioEmitterPose,
    settings: engine::audio::AudioVoiceSpatialSettings,
) -> engine::audio::StereoGains {
    engine::audio::spatial_voice_gains(listener, emitter, settings)
}

fn parse_emitter(value: &Value) -> Option<EmitterPreview> {
    let Value::Object(fields) = value else { return None; };
    let Value::AssetRef(clip) = fields.get("clip")? else { return None; };
    let rolloff = match fields.get("rolloff") {
        Some(Value::String(value)) if value == "inverse" => engine::audio::AudioRolloffMode::Inverse,
        _ => engine::audio::AudioRolloffMode::Linear,
    };
    Some(EmitterPreview {
        clip: clip.clone(),
        settings: engine::audio::AudioVoiceSpatialSettings {
            volume: number(fields.get("volume"), 1.0),
            spatial_blend: number(fields.get("spatial_blend"), 1.0),
            min_distance: number(fields.get("min_distance"), 1.0),
            max_distance: number(fields.get("max_distance"), 20.0),
            rolloff,
        },
        looping: matches!(fields.get("looping"), Some(Value::Bool(true))),
    })
}

fn active_game_listener(scene: &AuthoringScene) -> Option<EntityId> {
    let component = ComponentTypeId::new(engine::scene_bridge::AUDIO_LISTENER_COMPONENT);
    scene.entities().filter_map(|(id, entity)| {
        let Value::Object(fields) = entity.components.get(&component)? else { return None; };
        if !matches!(fields.get("enabled"), Some(Value::Bool(true))) { return None; }
        Some((integer(fields.get("priority"), 0), id.clone()))
    }).max_by(|(lp, li), (rp, ri)| lp.cmp(rp).then_with(|| ri.as_str().cmp(li.as_str()))).map(|(_, id)| id)
}

fn draw_attenuation_preview(ui: &mut egui::Ui, settings: engine::audio::AudioVoiceSpatialSettings) {
    let width = ui.available_width().max(80.0);
    let (rect, _) = ui.allocate_exact_size(egui::vec2(width, 72.0), egui::Sense::hover());
    let painter = ui.painter_at(rect);
    let max_distance = settings.max_distance.max(settings.min_distance).max(1.0);
    let listener = engine::audio::AudioListenerPose { position: [0.0; 3], right: [1.0, 0.0, 0.0] };
    let mut previous = None;
    for index in 0..=48 {
        let t = index as f32 / 48.0;
        let distance = max_distance * 1.2 * t;
        let emitter = engine::audio::AudioEmitterPose { position: [0.0, 0.0, -distance] };
        let gains = audition_spatial_gains(Some(listener), emitter, engine::audio::AudioVoiceSpatialSettings { volume: 1.0, spatial_blend: 1.0, ..settings });
        let gain = (gains.left * gains.left + gains.right * gains.right).sqrt().clamp(0.0, 1.0);
        let point = egui::pos2(rect.left() + rect.width() * t, rect.bottom() - rect.height() * gain);
        if let Some(previous) = previous {
            painter.line_segment([previous, point], egui::Stroke::new(1.5_f32, ui.visuals().text_color()));
        }
        previous = Some(point);
    }
}

fn number(value: Option<&Value>, default: f32) -> f32 {
    match value { Some(Value::F64(value)) => *value as f32, Some(Value::I64(value)) => *value as f32, Some(Value::U64(value)) => *value as f32, _ => default }
}

fn integer(value: Option<&Value>, default: i64) -> i64 {
    match value { Some(Value::I64(value)) => *value, Some(Value::U64(value)) => i64::try_from(*value).unwrap_or(i64::MAX), _ => default }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn audition_and_runtime_use_identical_spatial_result() {
        let listener = engine::audio::AudioListenerPose { position: [1.0, 0.0, 0.0], right: [1.0, 0.0, 0.0] };
        let emitter = engine::audio::AudioEmitterPose { position: [-2.0, 0.0, 1.0] };
        let settings = engine::audio::AudioVoiceSpatialSettings { volume: 0.75, spatial_blend: 0.8, min_distance: 1.0, max_distance: 12.0, rolloff: engine::audio::AudioRolloffMode::Inverse };
        assert_eq!(audition_spatial_gains(Some(listener), emitter, settings), engine::audio::spatial_voice_gains(Some(listener), emitter, settings));
    }

    #[test]
    fn listener_selection_uses_priority_then_stable_id() {
        let mut scene = AuthoringScene::new();
        let listener_type = ComponentTypeId::new(engine::scene_bridge::AUDIO_LISTENER_COMPONENT);
        let high_a = EntityId::generate();
        let high_b = EntityId::generate();
        let low = EntityId::generate();
        let expected = if high_a.as_str() < high_b.as_str() { high_a.clone() } else { high_b.clone() };
        let mut transaction = engine_authoring::Transaction::begin(&scene);
        for (id, priority) in [(high_a, 7_i64), (high_b, 7_i64), (low, 3_i64)] {
            transaction.apply(AuthoringCommand::CreateEntity { id: id.clone(), name: "listener".to_owned(), parent: None });
            transaction.apply(AuthoringCommand::AddComponent {
                entity: id,
                component_type: listener_type.clone(),
                value: Value::Object(std::collections::BTreeMap::from([
                    ("enabled".to_owned(), Value::Bool(true)),
                    ("priority".to_owned(), Value::I64(priority)),
                ])),
            });
        }
        transaction.commit(&mut scene).expect("listener fixture must commit");
        assert_eq!(active_game_listener(&scene), Some(expected));
    }
}
