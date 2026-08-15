//! Authored gameplay identity metadata retained in the runtime world.

/// Searchable name, tags, and team assigned to one runtime entity.
///
/// Stable authoring identity remains a separate higher-level runtime contract.
/// This component carries mutable gameplay classification that project code may
/// inspect without parsing display text.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RuntimeMetadata {
    /// Gameplay-facing name; defaults to the authoring entity name.
    pub name: String,
    /// Deterministic authored classification tags.
    pub tags: Vec<String>,
    /// Team or faction identifier used by targeting and combat rules.
    pub team: String,
}
