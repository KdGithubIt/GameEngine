//! Authoring-visible renderer budgets.
//!
//! The stable numeric budgets live in `engine-render-runtime`; this facade adds
//! scene-level validation without making lower render/import crates depend on
//! runtime scene composition.

use crate::scene_bridge::{
    AMBIENT_LIGHT_COMPONENT, DIRECTIONAL_LIGHT_COMPONENT, LOD_GROUP_COMPONENT,
    PARTICLE_EMITTER_COMPONENT, SKINNED_MESH_RENDERER_COMPONENT, STATIC_MESH_RENDERER_COMPONENT,
};
use engine_authoring::{AuthoringScene, ComponentTypeId, Diagnostic, DiagnosticTarget, Value};

pub use engine_render_runtime::render_limits::{
    MATERIAL_TEXTURE_SLOTS, MAX_AMBIENT_LIGHTS, MAX_DIRECTIONAL_LIGHTS,
    MAX_PARTICLES_PER_EMITTER, MAX_RENDER_INSTANCES, MAX_TEXTURE_DIMENSION,
};

/// Reports deterministic scene budgets before runtime/GPU preparation.
pub fn validate_scene_render_limits(scene: &AuthoringScene) -> Vec<Diagnostic> {
    let static_renderer = ComponentTypeId::new(STATIC_MESH_RENDERER_COMPONENT);
    let lod = ComponentTypeId::new(LOD_GROUP_COMPONENT);
    let skinned_renderer = ComponentTypeId::new(SKINNED_MESH_RENDERER_COMPONENT);
    let particles = ComponentTypeId::new(PARTICLE_EMITTER_COMPONENT);
    let directional = ComponentTypeId::new(DIRECTIONAL_LIGHT_COMPONENT);
    let ambient = ComponentTypeId::new(AMBIENT_LIGHT_COMPONENT);
    let mut diagnostics = Vec::new();
    let mut worst_case_instances = 0_usize;
    let mut directional_count = 0_usize;
    let mut ambient_count = 0_usize;

    for (entity_id, entity) in scene.entities() {
        if entity.components.contains_key(&static_renderer)
            || entity.components.contains_key(&lod)
            || entity.components.contains_key(&skinned_renderer)
        {
            worst_case_instances = worst_case_instances.saturating_add(1);
        }
        directional_count += usize::from(entity.components.contains_key(&directional));
        ambient_count += usize::from(entity.components.contains_key(&ambient));

        let Some(Value::Object(fields)) = entity.components.get(&particles) else {
            continue;
        };
        let Some(maximum) = fields.get("max_particles").and_then(value_as_usize) else {
            continue;
        };
        worst_case_instances = worst_case_instances.saturating_add(maximum);
        if maximum > MAX_PARTICLES_PER_EMITTER {
            diagnostics.push(
                Diagnostic::error(
                    "renderer.particle_emitter_limit",
                    format!(
                        "particle emitter requests {maximum} particles; the Editor Ready v1 limit is {MAX_PARTICLES_PER_EMITTER}"
                    ),
                )
                .with_target(DiagnosticTarget::Component {
                    entity: entity_id.clone(),
                    component_type: particles.clone(),
                }),
            );
        }
    }

    if worst_case_instances > MAX_RENDER_INSTANCES {
        diagnostics.push(Diagnostic::error(
            "renderer.instance_limit",
            format!(
                "scene can produce {worst_case_instances} render instances; the Editor Ready v1 limit is {MAX_RENDER_INSTANCES}"
            ),
        ));
    }
    push_light_limit(
        directional_count,
        MAX_DIRECTIONAL_LIGHTS,
        "directional",
        &mut diagnostics,
    );
    push_light_limit(
        ambient_count,
        MAX_AMBIENT_LIGHTS,
        "ambient",
        &mut diagnostics,
    );
    diagnostics
}

fn value_as_usize(value: &Value) -> Option<usize> {
    match value {
        Value::I64(value) => usize::try_from(*value).ok(),
        Value::U64(value) => usize::try_from(*value).ok(),
        _ => None,
    }
}

fn push_light_limit(actual: usize, maximum: usize, kind: &str, diagnostics: &mut Vec<Diagnostic>) {
    if actual > maximum {
        diagnostics.push(Diagnostic::warning(
            "renderer.light_limit",
            format!(
                "scene contains {actual} {kind} lights; only the first {maximum} in stable authoring order drives rendering"
            ),
        ));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn oversized_particle_pool_reports_emitter_and_scene_instance_budgets() {
        let scene = engine_authoring::test_fixtures::load_scene_fixture(&format!(
            r#"{{
                "entities": [{{
                    "id": "entity_01JP0000000000000000000001",
                    "name": "oversized",
                    "components": {{
                        "engine.particle_emitter": {{"max_particles": {}}}
                    }}
                }}]
            }}"#,
            MAX_RENDER_INSTANCES + 1
        ))
        .expect("budget fixture");

        let diagnostics = validate_scene_render_limits(&scene);
        assert!(diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code == "renderer.particle_emitter_limit"));
        assert!(diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code == "renderer.instance_limit"));
    }
}
