//! Compatibility facade for animation morph state plus render-side adapters.

pub use engine_animation::morph::*;
pub use engine_render_runtime::morph::MorphDirtyVertices;

use crate::material::Material;
use crate::mesh::Mesh;
use glam::Vec3;
use hashbrown::HashMap;

/// Applies each entity's [`MorphWeights`] to its private mesh in place.
pub fn morph_blend_system(
    mut meshes: engine_ecs::Query<(
        &mut Mesh,
        &MorphTargets,
        &MorphWeights,
        &mut MorphDirtyVertices,
    )>,
) {
    for (_, (mesh, targets, weights, dirty)) in meshes.iter_mut() {
        dirty.changed = apply_morph_blend(mesh, targets, weights);
    }
}

/// Computes one entity's blended positions and returns changed vertex indices.
pub fn apply_morph_blend(
    mesh: &mut Mesh,
    targets: &MorphTargets,
    weights: &MorphWeights,
) -> Vec<u32> {
    let mut changed = Vec::new();
    let mut blended: HashMap<u32, Vec3> = HashMap::with_capacity(targets.working_set().len());
    for vertex in targets.working_set() {
        let rest = targets
            .rest_positions()
            .get(*vertex as usize)
            .copied()
            .unwrap_or(Vec3::ZERO);
        blended.insert(*vertex, rest);
    }
    for (index, morph) in targets.morphs().iter().enumerate() {
        let weight = weights.get(index);
        if weight == 0.0 {
            continue;
        }
        for (vertex, delta) in &morph.vertex_deltas {
            if let Some(position) = blended.get_mut(vertex) {
                *position += *delta * weight;
            }
        }
    }
    for vertex in targets.working_set() {
        let Some(position) = blended.get(vertex) else {
            continue;
        };
        let Some(target) = mesh.vertices.get_mut(*vertex as usize) else {
            continue;
        };
        let updated = position.to_array();
        if target.position != updated {
            target.position = updated;
            changed.push(*vertex);
        }
    }
    changed
}

/// Applies active material morphs to authored base colors.
pub fn material_morph_system(
    mut materials: engine_ecs::Query<(
        &mut Material,
        &MorphBaseColor,
        &MorphTargets,
        &MorphWeights,
    )>,
) {
    for (_, (material, base, targets, weights)) in materials.iter_mut() {
        material.color = blended_base_color(base.0, targets, weights);
    }
}

/// Returns the rest positions used when binding morph targets to `mesh`.
pub fn rest_positions(mesh: &Mesh) -> Vec<Vec3> {
    mesh.vertices
        .iter()
        .map(|vertex| Vec3::from_array(vertex.position))
        .collect()
}
