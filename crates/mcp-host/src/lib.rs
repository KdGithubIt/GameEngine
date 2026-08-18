//! GUI-free MCP host infrastructure and authoritative headless authoring host.
//!
//! ADR 0151 keeps MCP transport separate from authoring semantics and grants a
//! headless writer authority only through the project-lifecycle OS lease.

#![warn(missing_docs)]
#![warn(rustdoc::broken_intra_doc_links)]

mod headless;
pub mod transport;

pub use headless::{
    HeadlessAccessMode, HeadlessAuthoringHost, HeadlessHostError, HeadlessMcpCallFailure,
    HeadlessProjectSelection, HeadlessViewDescriptor,
};
