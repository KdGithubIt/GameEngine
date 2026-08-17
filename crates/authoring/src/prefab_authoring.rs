//! Shared prefab instantiation semantics over the Scene authoring transaction API.
//!
//! The prefab schema owns reusable entity templates, while this service owns the
//! authoring command batch used to preview or apply one instance. Editor, CLI,
//! and MCP adapters must not add their own ID-remap or instance-marker rules.

use crate::{
    AuthoringCommand, AuthoringEntity, AuthoringPermissions, AuthoringSession, ComponentTypeId,
    EntityId, PrefabAsset, PrefabError, SceneAuthoringError, SceneAuthoringMutation,
    SceneAuthoringService, Value,
};
use serde::Serialize;
use std::collections::BTreeMap;
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};

/// Persisted editor-only component that records the source of a prefab instance.
pub const PREFAB_INSTANCE_COMPONENT: &str = "editor.prefab_instance";

/// Field of [`PREFAB_INSTANCE_COMPONENT`] that names the prefab document.
pub const PREFAB_INSTANCE_SOURCE_FIELD: &str = "source";

/// Portable location of a prefab document, relative to the project root.
///
/// [`PREFAB_INSTANCE_COMPONENT`] persists this string into `*.scene.json`, so
/// it becomes canonical project data and must survive moving the project to
/// another machine or checkout (ADR 0021, ADR 0134). The stored form is always
/// forward-slash separated and never absolute.
///
/// # Examples
///
/// ```
/// use engine_authoring::PrefabSourcePath;
/// use std::path::Path;
///
/// let source = PrefabSourcePath::from_project_path(
///     Path::new("/projects/demo"),
///     Path::new("/projects/demo/assets/prefabs/hero.prefab.json"),
/// )
/// .expect("a path inside the project is portable");
/// assert_eq!(source.as_str(), "assets/prefabs/hero.prefab.json");
/// ```
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct PrefabSourcePath(String);

impl PrefabSourcePath {
    /// Creates one portable source from a project-root-relative path.
    ///
    /// Both separators are accepted on every platform and `.` components are
    /// dropped, so a caller may pass a native path without pre-normalizing it.
    ///
    /// # Errors
    ///
    /// Returns [`PrefabSourceError::NotProjectRelative`] when `relative` is
    /// absolute, empty, not UTF-8, or contains a `..` component.
    pub fn from_project_relative(relative: impl AsRef<Path>) -> Result<Self, PrefabSourceError> {
        let relative = relative.as_ref();
        let invalid = || PrefabSourceError::NotProjectRelative(relative.to_path_buf());
        let Some(text) = relative.to_str() else {
            return Err(invalid());
        };
        if is_absolute_like(text) {
            return Err(invalid());
        }
        let mut parts = Vec::new();
        for part in text.split(['/', '\\']) {
            match part {
                "" | "." => {}
                ".." => return Err(invalid()),
                part => parts.push(part),
            }
        }
        if parts.is_empty() {
            return Err(invalid());
        }
        Ok(Self(parts.join("/")))
    }

    /// Creates one portable source by removing `project_root` from `path`.
    ///
    /// The two paths are compared lexically first. When that fails they are
    /// canonicalized once, so a project reached through a symlink, or a legacy
    /// marker written without the Windows verbatim prefix, still converts.
    ///
    /// # Errors
    ///
    /// Returns [`PrefabSourceError::OutsideProject`] when `path` does not live
    /// below `project_root`, or [`PrefabSourceError::NotProjectRelative`] when
    /// the remainder cannot be stored as portable project data.
    pub fn from_project_path(project_root: &Path, path: &Path) -> Result<Self, PrefabSourceError> {
        if let Ok(relative) = path.strip_prefix(project_root) {
            return Self::from_project_relative(relative);
        }
        if let (Ok(root), Ok(target)) = (fs::canonicalize(project_root), fs::canonicalize(path))
            && let Ok(relative) = target.strip_prefix(&root)
        {
            return Self::from_project_relative(relative);
        }
        Err(PrefabSourceError::OutsideProject {
            project_root: project_root.to_path_buf(),
            path: path.to_path_buf(),
        })
    }

    /// Returns the persisted forward-slash separated project-relative path.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Returns the filesystem path this source names inside `project_root`.
    ///
    /// Components are pushed individually so the result uses native separators
    /// even when `project_root` is a Windows verbatim path, which the operating
    /// system does not normalize.
    pub fn resolve(&self, project_root: &Path) -> PathBuf {
        let mut resolved = project_root.to_path_buf();
        for part in self.0.split('/') {
            resolved.push(part);
        }
        resolved
    }
}

impl fmt::Display for PrefabSourcePath {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// Failure converting a filesystem location into portable prefab project data.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PrefabSourceError {
    /// The path is absolute, empty, or escapes the project root.
    NotProjectRelative(PathBuf),
    /// The path does not live below the supplied project root.
    OutsideProject {
        /// Project root the path was compared against.
        project_root: PathBuf,
        /// Path that could not be made portable.
        path: PathBuf,
    },
}

impl PrefabSourceError {
    /// Returns the stable diagnostic-style code for this failure.
    pub fn code(&self) -> &'static str {
        match self {
            Self::NotProjectRelative(_) => "prefab.invalid_source_path",
            Self::OutsideProject { .. } => "prefab.source_outside_project",
        }
    }
}

impl fmt::Display for PrefabSourceError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotProjectRelative(path) => write!(
                formatter,
                "prefab source `{}` must be a project-relative path",
                path.display()
            ),
            Self::OutsideProject { project_root, path } => write!(
                formatter,
                "prefab source `{}` is outside project `{}`",
                path.display(),
                project_root.display()
            ),
        }
    }
}

impl std::error::Error for PrefabSourceError {}

/// Prefab location recorded on one instance root.
///
/// Current authoring code always writes [`Self::Portable`]. [`Self::Legacy`]
/// exists only so Scenes written before ADR 0134 keep opening; those documents
/// stored the authoring machine's absolute path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PrefabInstanceSource {
    /// Project-root-relative location written by current authoring code.
    Portable(PrefabSourcePath),
    /// Machine-specific absolute location written before ADR 0134.
    Legacy(PathBuf),
}

impl PrefabInstanceSource {
    /// Classifies one persisted `source` string.
    ///
    /// Returns `None` when the string is neither a portable project-relative
    /// path nor an absolute path, which means the marker cannot be resolved.
    pub fn parse(raw: &str) -> Option<Self> {
        match PrefabSourcePath::from_project_relative(raw) {
            Ok(source) => Some(Self::Portable(source)),
            Err(_) if is_absolute_like(raw) => Some(Self::Legacy(PathBuf::from(raw))),
            Err(_) => None,
        }
    }

    /// Returns the filesystem path to open for this source.
    pub fn resolve(&self, project_root: &Path) -> PathBuf {
        match self {
            Self::Portable(source) => source.resolve(project_root),
            Self::Legacy(path) => path.clone(),
        }
    }

    /// Returns the portable form, converting a legacy absolute path when it
    /// still points inside `project_root`.
    ///
    /// Callers that rewrite the marker use this so a legacy Scene becomes
    /// portable the first time its instance is reverted or re-instantiated.
    ///
    /// # Errors
    ///
    /// Returns [`PrefabSourceError`] when a legacy path cannot be expressed
    /// relative to `project_root`.
    pub fn to_portable(&self, project_root: &Path) -> Result<PrefabSourcePath, PrefabSourceError> {
        match self {
            Self::Portable(source) => Ok(source.clone()),
            Self::Legacy(path) => PrefabSourcePath::from_project_path(project_root, path),
        }
    }

    /// Returns whether this source is already portable project data.
    pub fn is_portable(&self) -> bool {
        matches!(self, Self::Portable(_))
    }
}

impl fmt::Display for PrefabInstanceSource {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Portable(source) => source.fmt(formatter),
            Self::Legacy(path) => write!(formatter, "{}", path.display()),
        }
    }
}

/// Builds the persisted [`PREFAB_INSTANCE_COMPONENT`] value for one source.
pub fn prefab_instance_marker(source: &PrefabSourcePath) -> Value {
    Value::Object(BTreeMap::from([(
        PREFAB_INSTANCE_SOURCE_FIELD.to_owned(),
        Value::String(source.as_str().to_owned()),
    )]))
}

/// Reads the prefab source recorded on one entity, if it is an instance root.
pub fn prefab_instance_source(entity: &AuthoringEntity) -> Option<PrefabInstanceSource> {
    let Value::Object(fields) = entity
        .components
        .get(&ComponentTypeId::new(PREFAB_INSTANCE_COMPONENT))?
    else {
        return None;
    };
    let Value::String(raw) = fields.get(PREFAB_INSTANCE_SOURCE_FIELD)? else {
        return None;
    };
    PrefabInstanceSource::parse(raw)
}

/// Reports whether `text` names a machine-specific absolute location.
///
/// Windows drive-qualified, UNC, and verbatim paths are lexically relative on
/// Unix, so a Scene authored on Windows would otherwise be misread as portable
/// project data when opened on another platform.
fn is_absolute_like(text: &str) -> bool {
    let path = Path::new(text);
    if path.is_absolute() || path.has_root() {
        return true;
    }
    let bytes = text.as_bytes();
    text.starts_with("\\\\")
        || (bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':')
}

/// Complete request metadata for one prefab instantiation attempt.
///
/// Keeping the source marker, parent override, and stale-Scene token together
/// prevents adapters from growing parallel method signatures as prefab
/// instantiation gains more structured options.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrefabInstantiationRequest {
    /// Portable prefab source persisted on the instantiated root marker.
    pub source: PrefabSourcePath,
    /// Optional Scene parent assigned to the freshly remapped prefab root.
    pub parent: Option<EntityId>,
    /// Scene revision used as the mutation base.
    pub expected_revision: u64,
    /// Scene generation used as the mutation base.
    pub expected_generation: u64,
}

impl PrefabInstantiationRequest {
    /// Creates one request from adapter-resolved prefab and Scene state.
    pub fn new(
        source: PrefabSourcePath,
        parent: Option<EntityId>,
        expected_revision: u64,
        expected_generation: u64,
    ) -> Self {
        Self {
            source,
            parent,
            expected_revision,
            expected_generation,
        }
    }
}

/// Result of previewing or applying one prefab instantiation.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct PrefabInstantiationMutation {
    /// Fresh root ID proposed for this independent instantiation call.
    pub proposed_root: EntityId,
    /// Shared Scene mutation result, including diagnostics and semantic diff.
    pub mutation: SceneAuthoringMutation,
}

/// Failure from shared prefab instantiation semantics.
#[derive(Debug)]
pub enum PrefabAuthoringError {
    /// The prefab definition cannot produce a valid command batch.
    Prefab(PrefabError),
    /// The shared Scene authoring service rejected the operation.
    Scene(SceneAuthoringError),
}

impl PrefabAuthoringError {
    /// Returns the stable diagnostic-style code for this failure.
    pub fn code(&self) -> &'static str {
        match self {
            Self::Prefab(_) => "prefab.invalid_definition",
            Self::Scene(source) => source.code(),
        }
    }
}

impl fmt::Display for PrefabAuthoringError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Prefab(source) => write!(formatter, "prefab definition is invalid: {source}"),
            Self::Scene(source) => source.fmt(formatter),
        }
    }
}

impl std::error::Error for PrefabAuthoringError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Prefab(source) => Some(source),
            Self::Scene(source) => Some(source),
        }
    }
}

impl From<PrefabError> for PrefabAuthoringError {
    fn from(source: PrefabError) -> Self {
        Self::Prefab(source)
    }
}

impl From<SceneAuthoringError> for PrefabAuthoringError {
    fn from(source: SceneAuthoringError) -> Self {
        Self::Scene(source)
    }
}

/// GUI-free prefab instantiation behavior shared by structured authoring clients.
#[derive(Debug, Default, Clone, Copy)]
pub struct PrefabAuthoringService;

impl PrefabAuthoringService {
    /// Creates the stateless prefab authoring service.
    pub fn new() -> Self {
        Self
    }

    /// Previews one prefab instantiation without changing the Scene or undo history.
    ///
    /// # Errors
    ///
    /// Returns [`PrefabAuthoringError`] when the prefab is structurally invalid,
    /// preview permission is absent, or the supplied revision/generation is stale.
    pub fn preview_instantiation(
        &self,
        session: &AuthoringSession,
        permissions: &AuthoringPermissions,
        prefab: &PrefabAsset,
        request: PrefabInstantiationRequest,
    ) -> Result<PrefabInstantiationMutation, PrefabAuthoringError> {
        let PrefabInstantiationRequest {
            source,
            parent,
            expected_revision,
            expected_generation,
        } = request;
        let (proposed_root, commands) = instantiation_commands(prefab, &source, parent)?;
        let mutation = SceneAuthoringService::new().preview(
            session,
            permissions,
            expected_revision,
            expected_generation,
            commands,
        )?;
        Ok(PrefabInstantiationMutation {
            proposed_root,
            mutation,
        })
    }

    /// Applies one prefab instantiation as one Scene transaction and undo entry.
    ///
    /// # Errors
    ///
    /// Returns [`PrefabAuthoringError`] when the prefab is structurally invalid,
    /// project-data-write permission is absent, the supplied revision/generation
    /// is stale, or the Scene transaction cannot be committed.
    pub fn apply_instantiation(
        &self,
        session: &mut AuthoringSession,
        permissions: &AuthoringPermissions,
        prefab: &PrefabAsset,
        request: PrefabInstantiationRequest,
    ) -> Result<PrefabInstantiationMutation, PrefabAuthoringError> {
        let PrefabInstantiationRequest {
            source,
            parent,
            expected_revision,
            expected_generation,
        } = request;
        let (proposed_root, commands) = instantiation_commands(prefab, &source, parent)?;
        let mutation = SceneAuthoringService::new().apply(
            session,
            permissions,
            expected_revision,
            expected_generation,
            commands,
        )?;
        Ok(PrefabInstantiationMutation {
            proposed_root,
            mutation,
        })
    }
}

fn instantiation_commands(
    prefab: &PrefabAsset,
    source: &PrefabSourcePath,
    parent: Option<EntityId>,
) -> Result<(EntityId, Vec<AuthoringCommand>), PrefabAuthoringError> {
    let mut instance = prefab.instantiate_with_root(parent)?;
    instance.commands.push(AuthoringCommand::AddComponent {
        entity: instance.root.clone(),
        component_type: ComponentTypeId::new(PREFAB_INSTANCE_COMPONENT),
        value: prefab_instance_marker(source),
    });
    Ok((instance.root, instance.commands))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{AuthoringEntity, AuthoringPermission, AuthoringScene};

    fn permissions() -> AuthoringPermissions {
        AuthoringPermissions::read_only()
            .with(AuthoringPermission::Preview)
            .with(AuthoringPermission::ProjectDataWrite)
    }

    fn prefab() -> PrefabAsset {
        let id = EntityId::generate();
        let entity = AuthoringEntity::new(id.clone(), "enemy");
        PrefabAsset::from_selection([(&id, &entity)]).expect("prefab fixture")
    }

    fn source() -> PrefabSourcePath {
        PrefabSourcePath::from_project_relative("assets/prefabs/enemy.prefab.json")
            .expect("fixture source is project-relative")
    }

    #[test]
    fn preview_is_non_destructive_and_reports_proposed_root() {
        let session = AuthoringSession::new(AuthoringScene::new());
        let base = SceneAuthoringService::new()
            .inspect(&session, &permissions())
            .expect("base snapshot");

        let result = PrefabAuthoringService::new()
            .preview_instantiation(
                &session,
                &permissions(),
                &prefab(),
                PrefabInstantiationRequest::new(source(), None, base.revision, base.generation),
            )
            .expect("preview");

        assert!(result.mutation.success);
        assert!(session.scene().entity(&result.proposed_root).is_none());
        assert!(!session.can_undo());
    }

    #[test]
    fn apply_is_one_undoable_scene_transaction_with_source_marker() {
        let mut session = AuthoringSession::new(AuthoringScene::new());
        let permissions = permissions();
        let base = SceneAuthoringService::new()
            .inspect(&session, &permissions)
            .expect("base snapshot");

        let result = PrefabAuthoringService::new()
            .apply_instantiation(
                &mut session,
                &permissions,
                &prefab(),
                PrefabInstantiationRequest::new(source(), None, base.revision, base.generation),
            )
            .expect("apply");

        assert!(result.mutation.success);
        let root = session
            .scene()
            .entity(&result.proposed_root)
            .expect("instantiated root");
        assert!(root
            .components
            .contains_key(&ComponentTypeId::new(PREFAB_INSTANCE_COMPONENT)));
        assert!(session.can_undo());
        assert!(session.undo());
        assert!(session.scene().entity(&result.proposed_root).is_none());
    }

    #[test]
    fn instantiated_root_persists_a_project_relative_source() {
        let mut session = AuthoringSession::new(AuthoringScene::new());
        let permissions = permissions();
        let base = SceneAuthoringService::new()
            .inspect(&session, &permissions)
            .expect("base snapshot");

        let result = PrefabAuthoringService::new()
            .apply_instantiation(
                &mut session,
                &permissions,
                &prefab(),
                PrefabInstantiationRequest::new(source(), None, base.revision, base.generation),
            )
            .expect("apply");

        let root = session
            .scene()
            .entity(&result.proposed_root)
            .expect("instantiated root");
        assert_eq!(
            prefab_instance_source(root),
            Some(PrefabInstanceSource::Portable(source())),
            "the persisted marker must stay portable across machines"
        );
    }

    #[test]
    fn stale_base_is_forwarded_from_scene_service() {
        let mut session = AuthoringSession::new(AuthoringScene::new());
        let permissions = permissions();
        let base = SceneAuthoringService::new()
            .inspect(&session, &permissions)
            .expect("base snapshot");
        let first = PrefabAuthoringService::new()
            .apply_instantiation(
                &mut session,
                &permissions,
                &prefab(),
                PrefabInstantiationRequest::new(source(), None, base.revision, base.generation),
            )
            .expect("first apply");
        assert!(first.mutation.success);

        let error = PrefabAuthoringService::new()
            .apply_instantiation(
                &mut session,
                &permissions,
                &prefab(),
                PrefabInstantiationRequest::new(source(), None, base.revision, base.generation),
            )
            .expect_err("stale apply must fail");
        assert_eq!(error.code(), "authoring.stale_revision");
    }

    #[test]
    fn portable_source_rejects_machine_specific_paths() {
        for absolute in [
            "/projects/demo/assets/prefabs/enemy.prefab.json",
            "C:\\projects\\demo\\assets\\prefabs\\enemy.prefab.json",
            "\\\\?\\C:\\projects\\demo\\assets\\prefabs\\enemy.prefab.json",
        ] {
            let error = PrefabSourcePath::from_project_relative(absolute)
                .expect_err("an absolute path must not become portable project data");
            assert_eq!(error.code(), "prefab.invalid_source_path");
        }

        let escaping = PrefabSourcePath::from_project_relative("../outside.prefab.json")
            .expect_err("a path escaping the project must be rejected");
        assert_eq!(escaping.code(), "prefab.invalid_source_path");

        let outside = PrefabSourcePath::from_project_path(
            Path::new("/projects/demo"),
            Path::new("/elsewhere/enemy.prefab.json"),
        )
        .expect_err("a path outside the project must be rejected");
        assert_eq!(outside.code(), "prefab.source_outside_project");
    }

    #[test]
    fn portable_source_normalizes_separators_and_resolves_natively() {
        let source = PrefabSourcePath::from_project_relative("assets\\prefabs\\.\\enemy.prefab.json")
            .expect("native separators are accepted");
        assert_eq!(source.as_str(), "assets/prefabs/enemy.prefab.json");
        assert_eq!(
            source.resolve(Path::new("/projects/demo")),
            Path::new("/projects/demo")
                .join("assets")
                .join("prefabs")
                .join("enemy.prefab.json")
        );
    }

    #[test]
    fn legacy_absolute_marker_stays_readable_and_converts_when_rewritten() {
        let absolute = Path::new("/projects/demo/assets/prefabs/enemy.prefab.json");
        let mut entity = AuthoringEntity::new(EntityId::generate(), "instance");
        entity.components.insert(
            ComponentTypeId::new(PREFAB_INSTANCE_COMPONENT),
            Value::Object(BTreeMap::from([(
                PREFAB_INSTANCE_SOURCE_FIELD.to_owned(),
                Value::String(absolute.display().to_string()),
            )])),
        );

        let recorded = prefab_instance_source(&entity).expect("legacy marker must stay readable");
        assert!(!recorded.is_portable());
        assert_eq!(recorded.resolve(Path::new("/other/project")), absolute);
        assert_eq!(
            recorded
                .to_portable(Path::new("/projects/demo"))
                .expect("a legacy path inside the project converts"),
            source()
        );
    }
}
