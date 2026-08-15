//! Compatibility facade for the PMX importer.

pub use engine_import::pmx_import::*;

#[cfg(test)]
mod tests {
    #[test]
    fn imported_and_runtime_mmd_scales_match() {
        assert_eq!(
            engine_import::pmx_import::PMX_TO_METERS,
            crate::mmd_physics::PMX_AUTHORING_SCALE,
        );
    }
}
