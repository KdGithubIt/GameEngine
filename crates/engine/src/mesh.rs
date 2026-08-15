//! Compatibility facade for render-runtime mesh APIs.

pub use engine_render_runtime::mesh::*;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mesh_to_obj_round_trips_through_the_engines_own_obj_loader() {
        let mesh = extract_baked_submesh(
            &Mesh::cube(),
            Submesh {
                start: 0,
                count: 36,
            },
        );
        let text = mesh_to_obj(&mesh);

        let directory = tempfile::tempdir().expect("temp dir");
        let path = directory.path().join("baked.obj");
        std::fs::write(&path, &text).expect("write obj");

        let loaded = crate::asset::load_obj(&path).expect("engine must read back its own bake");

        assert_eq!(loaded.vertices.len(), mesh.vertices.len());
        assert_eq!(
            loaded.indices.as_ref().map(Vec::len),
            mesh.indices.as_ref().map(Vec::len)
        );
        assert!(loaded.skinning.is_none());
        for (loaded_vertex, original_vertex) in loaded.vertices.iter().zip(&mesh.vertices) {
            for axis in 0..3 {
                assert!(
                    (loaded_vertex.position[axis] - original_vertex.position[axis]).abs() < 1.0e-4
                );
                assert!((loaded_vertex.normal[axis] - original_vertex.normal[axis]).abs() < 1.0e-4);
            }
            for axis in 0..2 {
                assert!(
                    (loaded_vertex.uv[axis] - original_vertex.uv[axis]).abs() < 1.0e-4,
                    "UV must round-trip through the writer's V-flip and the loader's V-flip"
                );
            }
        }
    }
}
