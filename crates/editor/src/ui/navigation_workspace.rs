//! Non-blocking Editor workflow for the shared production navigation bake service.

use crate::navmesh_bake::{
    bake_scene_navmesh, NavMeshBakeDocument, NavMeshBakeError, NavMeshBakeResult,
    NavigationBakeServiceError,
};
use crate::workspace::WorkspaceTabId;
use engine::AssetManifest;
use engine_authoring::{replace_file_contents, AuthoringScene, ProjectRoot};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver};
use std::sync::Arc;
use std::thread;

pub(super) enum NavigationBakeCompletion {
    Succeeded {
        tab_id: WorkspaceTabId,
        project: ProjectRoot,
        manifest: AssetManifest,
        result: Box<NavMeshBakeResult>,
    },
    Cancelled {
        tab_id: WorkspaceTabId,
    },
    Failed {
        tab_id: WorkspaceTabId,
        error: String,
    },
}

#[derive(Default)]
pub(super) struct NavigationBakeManager {
    receiver: Option<Receiver<NavigationBakeCompletion>>,
    cancelled: Option<Arc<AtomicBool>>,
    cancel_requested: bool,
}

impl NavigationBakeManager {
    pub(super) fn is_running(&self) -> bool {
        self.receiver.is_some()
    }

    pub(super) fn is_cancelling(&self) -> bool {
        self.is_running() && self.cancel_requested
    }

    pub(super) fn start(
        &mut self,
        tab_id: WorkspaceTabId,
        scene: AuthoringScene,
        project: ProjectRoot,
        manifest: AssetManifest,
        document: NavMeshBakeDocument,
        document_path: PathBuf,
    ) -> Result<(), &'static str> {
        if self.is_running() {
            return Err("a navigation bake is already running");
        }
        let cancelled = Arc::new(AtomicBool::new(false));
        let worker_cancelled = Arc::clone(&cancelled);
        let (sender, receiver) = mpsc::channel();
        self.receiver = Some(receiver);
        self.cancelled = Some(cancelled);
        self.cancel_requested = false;
        thread::spawn(move || {
            let completion = run_navigation_bake(
                tab_id,
                scene,
                project,
                manifest,
                document,
                document_path,
                &worker_cancelled,
            );
            let _ = sender.send(completion);
        });
        Ok(())
    }

    pub(super) fn poll(&mut self) -> Option<NavigationBakeCompletion> {
        let completion = self.receiver.as_ref()?.try_recv().ok()?;
        self.receiver = None;
        self.cancelled = None;
        self.cancel_requested = false;
        Some(completion)
    }

    pub(super) fn cancel(&mut self) -> bool {
        let Some(cancelled) = &self.cancelled else {
            return false;
        };
        cancelled.store(true, Ordering::Relaxed);
        self.cancel_requested = true;
        true
    }

    pub(super) fn clear(&mut self) {
        let _ = self.cancel();
        self.receiver = None;
        self.cancelled = None;
        self.cancel_requested = false;
    }
}

fn run_navigation_bake(
    tab_id: WorkspaceTabId,
    scene: AuthoringScene,
    project: ProjectRoot,
    mut manifest: AssetManifest,
    mut document: NavMeshBakeDocument,
    document_path: PathBuf,
    cancelled: &AtomicBool,
) -> NavigationBakeCompletion {
    let result = match bake_scene_navmesh(
        &scene,
        &project,
        &mut manifest,
        &mut document,
        cancelled,
    ) {
        Ok(result) => result,
        Err(NavMeshBakeError::Shared(NavigationBakeServiceError::Cancelled)) => {
            return NavigationBakeCompletion::Cancelled { tab_id };
        }
        Err(error) => {
            return NavigationBakeCompletion::Failed {
                tab_id,
                error: error.to_string(),
            };
        }
    };
    let persist_result = document
        .to_canonical_json()
        .map_err(|error| error.to_string())
        .and_then(|json| {
            if let Some(parent) = document_path.parent() {
                std::fs::create_dir_all(parent).map_err(|error| error.to_string())?;
            }
            replace_file_contents(&document_path, &json).map_err(|error| error.to_string())
        });
    if let Err(error) = persist_result {
        return NavigationBakeCompletion::Failed { tab_id, error };
    }
    NavigationBakeCompletion::Succeeded {
        tab_id,
        project,
        manifest,
        result: Box::new(result),
    }
}
