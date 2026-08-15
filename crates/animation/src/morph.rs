//! Animation-side morph assets, bindings, and per-entity weights.

use engine_authoring::id::AssetId;
use engine_authoring::LinearRgba;
use glam::Vec3;
use std::sync::Arc;

/// How a material morph combines its value with the material's own value.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum MaterialMorphOperation {
    /// Blend toward a multiplicative factor.
    #[default]
    Multiply,
    /// Add the morph value scaled by weight.
    Add,
}

/// One material's parameter override within a [`MorphAsset`].
#[derive(Debug, Clone, PartialEq)]
pub struct MaterialMorphOffset {
    /// Slot index within the renderer's material slots, or `None` for primary.
    pub slot: Option<usize>,
    /// Combination operation.
    pub operation: MaterialMorphOperation,
    /// Base color factor or addend.
    pub base_color: LinearRgba,
}

/// One imported named deformation of a single mesh.
#[derive(Debug, Clone, PartialEq)]
pub struct MorphAsset {
    /// Source selector paired with the owning mesh selector.
    pub source_index: usize,
    /// Imported stable sub-asset ID.
    pub id: AssetId,
    /// Human-readable morph name used by motion sources.
    pub name: String,
    /// Sparse position deltas `(vertex_index, delta)`.
    pub vertex_deltas: Vec<(u32, Vec3)>,
    /// Material parameter overrides.
    pub material_offsets: Vec<MaterialMorphOffset>,
}

impl MorphAsset {
    /// Returns `true` when this morph changes no vertex position.
    pub fn is_material_only(&self) -> bool {
        self.vertex_deltas.is_empty()
    }
}

/// Morph set bound to one render part plus immutable rest positions.
#[derive(Debug, Clone)]
pub struct MorphTargets {
    targets: Arc<MorphTargetData>,
}

#[derive(Debug)]
struct MorphTargetData {
    morphs: Vec<MorphAsset>,
    rest_positions: Vec<Vec3>,
    working_set: Vec<u32>,
}

impl MorphTargets {
    /// Binds `morphs` to a vertex array whose rest positions are `rest_positions`.
    pub fn new(morphs: Vec<MorphAsset>, rest_positions: Vec<Vec3>) -> Self {
        let vertex_count = rest_positions.len() as u32;
        let morphs = morphs
            .into_iter()
            .map(|mut morph| {
                morph.vertex_deltas.retain(|(vertex, _)| *vertex < vertex_count);
                morph
            })
            .collect::<Vec<_>>();
        let mut working_set = morphs
            .iter()
            .flat_map(|morph| morph.vertex_deltas.iter().map(|(vertex, _)| *vertex))
            .collect::<Vec<_>>();
        working_set.sort_unstable();
        working_set.dedup();
        Self {
            targets: Arc::new(MorphTargetData {
                morphs,
                rest_positions,
                working_set,
            }),
        }
    }

    /// Returns the bound morphs in sub-asset order.
    pub fn morphs(&self) -> &[MorphAsset] {
        &self.targets.morphs
    }

    /// Returns the union of every bound morph's vertex set.
    pub fn working_set(&self) -> &[u32] {
        &self.targets.working_set
    }

    /// Returns immutable rest positions used by render adapters for blending.
    pub fn rest_positions(&self) -> &[Vec3] {
        &self.targets.rest_positions
    }

    /// Returns the index of a morph named `name`, if bound.
    pub fn index_of(&self, name: &str) -> Option<usize> {
        self.targets.morphs.iter().position(|morph| morph.name == name)
    }

    /// Returns the index of a morph with stable ID `id`, if bound.
    pub fn index_of_id(&self, id: &AssetId) -> Option<usize> {
        self.targets.morphs.iter().position(|morph| &morph.id == id)
    }
}

/// Per-entity morph weights written by animation and consumed by render adapters.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct MorphWeights {
    /// Weight per bound morph, parallel to [`MorphTargets::morphs`].
    pub weights: Vec<f32>,
}

impl MorphWeights {
    /// Creates a zeroed weight vector sized for `targets`.
    pub fn for_targets(targets: &MorphTargets) -> Self {
        Self {
            weights: vec![0.0; targets.morphs().len()],
        }
    }
    /// Returns the weight at `index`, or zero when unset.
    pub fn get(&self, index: usize) -> f32 {
        self.weights.get(index).copied().unwrap_or(0.0)
    }
    /// Sets the weight at `index`, growing the vector if needed.
    pub fn set(&mut self, index: usize, weight: f32) {
        if index >= self.weights.len() {
            self.weights.resize(index + 1, 0.0);
        }
        self.weights[index] = weight;
    }
    /// Sets every weight to zero without reallocating.
    pub fn clear(&mut self) {
        self.weights.fill(0.0);
    }
    /// Returns `true` when no morph carries a non-zero weight.
    pub fn is_rest(&self) -> bool {
        self.weights.iter().all(|weight| *weight == 0.0)
    }
}

/// One morphing entity's authored base color captured at spawn.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MorphBaseColor(pub [f32; 4]);

/// Folds every active material morph into `base` without depending on render types.
pub fn blended_base_color(
    base: [f32; 4],
    targets: &MorphTargets,
    weights: &MorphWeights,
) -> [f32; 4] {
    let mut color = base;
    for (index, morph) in targets.morphs().iter().enumerate() {
        let weight = weights.get(index);
        if weight == 0.0 {
            continue;
        }
        for offset in &morph.material_offsets {
            let value = [
                offset.base_color.r,
                offset.base_color.g,
                offset.base_color.b,
                offset.base_color.a,
            ];
            for (channel, target) in color.iter_mut().enumerate() {
                *target = match offset.operation {
                    MaterialMorphOperation::Multiply => {
                        *target + (*target * value[channel] - *target) * weight
                    }
                    MaterialMorphOperation::Add => *target + value[channel] * weight,
                };
            }
        }
    }
    color
}

#[cfg(test)]
mod tests {
    use super::*;

    fn morph(name: &str, deltas: &[(u32, [f32; 3])]) -> MorphAsset {
        MorphAsset {
            source_index: 0,
            id: AssetId::generate(),
            name: name.to_owned(),
            vertex_deltas: deltas
                .iter()
                .map(|(vertex, delta)| (*vertex, Vec3::from_array(*delta)))
                .collect(),
            material_offsets: Vec::new(),
        }
    }

    #[test]
    fn working_set_is_deduplicated_and_out_of_range_deltas_are_dropped() {
        let targets = MorphTargets::new(
            vec![morph("a", &[(1, [1.0; 3]), (99, [1.0; 3])]), morph("b", &[(1, [1.0; 3]), (2, [1.0; 3])])],
            vec![Vec3::ZERO; 3],
        );
        assert_eq!(targets.working_set(), &[1, 2]);
        assert_eq!(targets.morphs()[0].vertex_deltas.len(), 1);
    }

    #[test]
    fn weights_grow_and_clear_without_clamping() {
        let targets = MorphTargets::new(vec![morph("a", &[])], Vec::new());
        let mut weights = MorphWeights::for_targets(&targets);
        weights.set(2, 1.5);
        assert_eq!(weights.get(2), 1.5);
        weights.clear();
        assert!(weights.is_rest());
    }
}
