//! Runtime scene-switch request state independent of world spawning policy.

use engine_ecs::Entity;

/// Tracks the currently loaded scene and any pending switch request.
///
/// This type owns only lifecycle bookkeeping. Loading, despawning, and scene
/// bridging remain composition-level operations above `engine-scene`.
#[derive(Debug, Default)]
pub struct SceneManager {
    pending: Option<String>,
    current_path: Option<String>,
    current_entities: Vec<Entity>,
    generation: u64,
}

impl SceneManager {
    /// Creates an empty manager with no current scene and no pending request.
    pub fn new() -> Self {
        Self::default()
    }

    /// Requests a switch to `path` at the next composition-layer scene boundary.
    ///
    /// Only the most recent distinct request is retained. A request for the
    /// already-current scene, or a duplicate pending request, is ignored.
    pub fn request_switch(&mut self, path: impl Into<String>) {
        let path = path.into();
        if self.pending.as_deref() == Some(path.as_str())
            || (self.pending.is_none() && self.current_path.as_deref() == Some(path.as_str()))
        {
            log::debug!("ignored duplicate scene switch request for `{path}`");
            return;
        }
        if let Some(discarded) = self.pending.replace(path) {
            log::warn!(
                "SceneManager::request_switch overwrote a pending request for `{discarded}`; \
                 only the most recent request made before the next scene boundary will execute"
            );
        }
    }

    /// Returns the number of scene switches completed successfully.
    pub fn generation(&self) -> u64 {
        self.generation
    }

    /// Returns the project-relative path of the currently loaded scene.
    pub fn current_scene_path(&self) -> Option<&str> {
        self.current_path.as_deref()
    }

    /// Returns the scene queued for the next frame boundary, if any.
    pub fn pending_scene_path(&self) -> Option<&str> {
        self.pending.as_deref()
    }

    /// Records a scene spawned directly by a host as the current scene.
    ///
    /// Startup registration does not increment [`Self::generation`].
    pub fn register_initial_scene(&mut self, path: impl Into<String>, entities: Vec<Entity>) {
        self.current_path = Some(path.into());
        self.current_entities = entities;
    }

    /// Takes the pending request for the composition-layer switch processor.
    #[doc(hidden)]
    pub fn take_pending_request(&mut self) -> Option<String> {
        self.pending.take()
    }

    /// Takes ownership of the entities recorded for the current scene.
    #[doc(hidden)]
    pub fn take_current_entities(&mut self) -> Vec<Entity> {
        std::mem::take(&mut self.current_entities)
    }

    /// Records a successfully spawned replacement scene and advances generation.
    #[doc(hidden)]
    pub fn complete_switch(&mut self, path: String, entities: Vec<Entity>) {
        self.current_entities = entities;
        self.current_path = Some(path);
        self.generation = self.generation.wrapping_add(1);
    }
}

/// Outcome of the most recently processed scene switch request.
#[derive(Debug, Clone, Default)]
pub enum SceneSwitchState {
    /// No switch is in progress; the most recent switch succeeded or none ran.
    #[default]
    Idle,
    /// The most recent switch attempt failed.
    Failed {
        /// The project-relative path that failed to load or spawn.
        path: String,
        /// Human-readable diagnostic text for logs and host UI.
        message: String,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn duplicate_current_and_pending_requests_are_ignored() {
        let mut manager = SceneManager::new();
        manager.register_initial_scene("scenes/a.scene.json", Vec::new());
        manager.request_switch("scenes/a.scene.json");
        assert!(manager.pending_scene_path().is_none());

        manager.request_switch("scenes/b.scene.json");
        manager.request_switch("scenes/b.scene.json");
        assert_eq!(manager.take_pending_request().as_deref(), Some("scenes/b.scene.json"));
    }

    #[test]
    fn complete_switch_replaces_entities_and_advances_generation() {
        let mut manager = SceneManager::new();
        let old = Entity::from_raw(1, 0);
        let new = Entity::from_raw(2, 0);
        manager.register_initial_scene("scenes/a.scene.json", vec![old]);

        assert_eq!(manager.take_current_entities(), vec![old]);
        manager.complete_switch("scenes/b.scene.json".to_owned(), vec![new]);

        assert_eq!(manager.generation(), 1);
        assert_eq!(manager.current_scene_path(), Some("scenes/b.scene.json"));
        assert_eq!(manager.take_current_entities(), vec![new]);
    }
}
