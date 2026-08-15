//! Low-dependency runtime asset identity, manifests, and reusable project data.

#![warn(missing_docs)]
#![warn(rustdoc::broken_intra_doc_links)]

/// Runtime asset handles and persisted import metadata.
pub mod asset;
/// Generic author-owned data assets and stable component references.
pub mod data_asset;
/// Content-addressed storage for disposable derived assets.
pub mod derived_cache;
/// Format-independent model intermediate representation.
pub mod model_ir;
