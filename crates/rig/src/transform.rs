use std::collections::{HashMap, HashSet};

use engine_ecs::{Entity, Query};
use glam::{Mat4, Quat, Vec3};

/// A local-space translation, rotation, and scale component.
#[derive(Debug, Clone, PartialEq)]
pub struct Transform {
    /// The local translation.
    pub translation: Vec3,
    /// The local rotation.
    pub rotation: Quat,
    /// The local scale.
    pub scale: Vec3,
}

impl Transform {
    /// Creates a transform with `translation` and identity rotation and scale.
    pub fn from_translation(translation: Vec3) -> Self {
        Self {
            translation,
            ..Self::default()
        }
    }

    /// Creates a transform with `rotation` and identity translation and scale.
    pub fn from_rotation(rotation: Quat) -> Self {
        Self {
            rotation,
            ..Self::default()
        }
    }

    /// Creates a transform at `eye` that looks toward `target`.
    ///
    /// `eye` and `target` should not be equal, and `up` should not be parallel
    /// to the viewing direction.
    pub fn looking_at(eye: Vec3, target: Vec3, up: Vec3) -> Self {
        let direction = (target - eye).normalize();
        let rotation = Quat::from_mat4(&Mat4::look_to_rh(eye, direction, up)).inverse();
        Self {
            translation: eye,
            rotation,
            scale: Vec3::ONE,
        }
    }

    /// Returns the local transformation matrix.
    pub fn to_matrix(&self) -> Mat4 {
        Mat4::from_scale_rotation_translation(self.scale, self.rotation, self.translation)
    }
}

impl Default for Transform {
    fn default() -> Self {
        Self {
            translation: Vec3::ZERO,
            rotation: Quat::IDENTITY,
            scale: Vec3::ONE,
        }
    }
}

/// A world-space transformation matrix component.
///
/// Built-in transform systems update this value from [`Transform`].
#[derive(Debug, Clone, PartialEq)]
pub struct GlobalTransform(pub Mat4);

impl GlobalTransform {
    /// Returns an identity global transform.
    pub fn identity() -> Self {
        Self(Mat4::IDENTITY)
    }

    /// Returns the world-space matrix.
    pub fn matrix(&self) -> Mat4 {
        self.0
    }
}

impl Default for GlobalTransform {
    fn default() -> Self {
        Self::identity()
    }
}

/// Identifies the parent entity in a transform hierarchy.
#[derive(Debug, Clone)]
pub struct Parent(
    /// The runtime entity that owns this entity.
    pub engine_ecs::Entity,
);

/// Stores the ordered child entities in a transform hierarchy.
#[derive(Debug, Clone)]
pub struct Children(
    /// Runtime child entities in hierarchy order.
    pub Vec<engine_ecs::Entity>,
);

/// Updates every [`GlobalTransform`] from the [`Transform`] hierarchy.
///
/// Entities without a [`Parent`] use their local matrix as the world matrix.
/// Entities with a [`Parent`] use the parent's resolved world matrix
/// multiplied by their local matrix. Resolution is memoized and depends only
/// on the parent links, so the result is deterministic regardless of entity
/// iteration order.
///
/// Invalid hierarchy state degrades instead of failing:
///
/// - A parent that is missing from the query (despawned, or lacking
///   [`Transform`] or [`GlobalTransform`]) is treated as a root boundary; the
///   child resolves as if it had no parent.
/// - Entities that form a parent cycle fall back to their local matrix.
///   Descendants outside the cycle still follow the fallback matrices, and
///   propagation never panics or loops forever.
pub fn transform_propagation_system(
    mut query: Query<(&Transform, &mut GlobalTransform, Option<&Parent>)>,
) {
    let mut nodes: HashMap<Entity, (Mat4, Option<Entity>)> = HashMap::new();
    for (entity, (transform, _, parent)) in query.iter_mut() {
        nodes.insert(
            entity,
            (transform.to_matrix(), parent.map(|parent| parent.0)),
        );
    }

    let cycle_members = collect_cycle_members(&nodes);
    let mut resolved: HashMap<Entity, Mat4> = HashMap::with_capacity(nodes.len());
    for &entity in nodes.keys() {
        resolve_world_matrix(entity, &nodes, &cycle_members, &mut resolved);
    }

    for (entity, (_, global, _)) in query.iter_mut() {
        if let Some(matrix) = resolved.get(&entity) {
            global.0 = *matrix;
        }
    }
}

/// Returns every entity that lies on a parent cycle.
///
/// Each parent chain is walked once. The cycle set is a property of the
/// parent links alone, which keeps the propagation fallback deterministic.
fn collect_cycle_members(nodes: &HashMap<Entity, (Mat4, Option<Entity>)>) -> HashSet<Entity> {
    #[derive(Clone, Copy, PartialEq)]
    enum VisitState {
        InChain,
        Finished,
    }

    let mut states: HashMap<Entity, VisitState> = HashMap::with_capacity(nodes.len());
    let mut cycle_members = HashSet::new();
    for &start in nodes.keys() {
        if states.contains_key(&start) {
            continue;
        }
        let mut chain = Vec::new();
        let mut current = start;
        loop {
            match states.get(&current) {
                Some(VisitState::InChain) => {
                    // The walk re-entered its own chain, so every entity from
                    // the first occurrence of `current` onward lies on a cycle.
                    let entry = chain
                        .iter()
                        .position(|&entity| entity == current)
                        .expect("in-chain state implies membership in the active chain");
                    cycle_members.extend(chain[entry..].iter().copied());
                    break;
                }
                Some(VisitState::Finished) => break,
                None => {}
            }
            states.insert(current, VisitState::InChain);
            chain.push(current);
            match nodes.get(&current).and_then(|(_, parent)| *parent) {
                Some(parent) if nodes.contains_key(&parent) => current = parent,
                _ => break,
            }
        }
        for entity in chain {
            states.insert(entity, VisitState::Finished);
        }
    }
    cycle_members
}

/// Resolves the world matrix of `entity` and every unresolved ancestor.
fn resolve_world_matrix(
    entity: Entity,
    nodes: &HashMap<Entity, (Mat4, Option<Entity>)>,
    cycle_members: &HashSet<Entity>,
    resolved: &mut HashMap<Entity, Mat4>,
) {
    let mut chain = Vec::new();
    let mut base = Mat4::IDENTITY;
    let mut current = entity;
    loop {
        if let Some(matrix) = resolved.get(&current) {
            base = *matrix;
            break;
        }
        chain.push(current);
        // Cycle members ignore their parent link and resolve as roots.
        if cycle_members.contains(&current) {
            break;
        }
        match nodes.get(&current).and_then(|(_, parent)| *parent) {
            Some(parent) if nodes.contains_key(&parent) => current = parent,
            _ => break,
        }
    }
    for &link in chain.iter().rev() {
        if let Some((local, _)) = nodes.get(&link) {
            base *= *local;
            resolved.insert(link, base);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::f32::consts::FRAC_PI_2;

    use engine_ecs::World;

    use super::*;

    fn spawn_node(world: &mut World, translation: Vec3, parent: Option<Entity>) -> Entity {
        let entity = world
            .spawn_with(Transform::from_translation(translation))
            .expect("test entity must spawn");
        world
            .add_component(entity, GlobalTransform::default())
            .expect("test entity must accept GlobalTransform");
        if let Some(parent) = parent {
            world
                .add_component(entity, Parent(parent))
                .expect("test entity must accept Parent");
        }
        entity
    }

    fn run_propagation(world: &mut World) {
        transform_propagation_system(Query::new(world));
    }

    fn global_translation(world: &World, entity: Entity) -> Vec3 {
        world
            .get_component::<GlobalTransform>(entity)
            .expect("test entity must keep GlobalTransform")
            .matrix()
            .col(3)
            .truncate()
    }

    #[test]
    fn root_transform_propagates_to_global_transform() {
        let mut world = World::new();
        let root = spawn_node(&mut world, Vec3::new(1.0, 2.0, 3.0), None);

        run_propagation(&mut world);

        assert_eq!(global_translation(&world, root), Vec3::new(1.0, 2.0, 3.0));
    }

    #[test]
    fn child_follows_translated_parent() {
        let mut world = World::new();
        let parent = spawn_node(&mut world, Vec3::new(10.0, 0.0, 0.0), None);
        let child = spawn_node(&mut world, Vec3::new(0.0, 5.0, 0.0), Some(parent));

        run_propagation(&mut world);

        assert_eq!(global_translation(&world, child), Vec3::new(10.0, 5.0, 0.0));
    }

    #[test]
    fn multi_level_hierarchy_accumulates_translations() {
        let mut world = World::new();
        let root = spawn_node(&mut world, Vec3::new(1.0, 0.0, 0.0), None);
        let middle = spawn_node(&mut world, Vec3::new(0.0, 2.0, 0.0), Some(root));
        let leaf = spawn_node(&mut world, Vec3::new(0.0, 0.0, 3.0), Some(middle));

        run_propagation(&mut world);

        assert_eq!(global_translation(&world, leaf), Vec3::new(1.0, 2.0, 3.0));
    }

    #[test]
    fn parent_rotation_rotates_child_offset() {
        let mut world = World::new();
        let parent = spawn_node(&mut world, Vec3::ZERO, None);
        {
            let transform = world
                .get_component_mut::<Transform>(parent)
                .expect("parent must keep Transform");
            transform.rotation = Quat::from_rotation_y(FRAC_PI_2);
        }
        let child = spawn_node(&mut world, Vec3::new(1.0, 0.0, 0.0), Some(parent));

        run_propagation(&mut world);

        let translation = global_translation(&world, child);
        assert!((translation - Vec3::new(0.0, 0.0, -1.0)).length() < 1.0e-5);
    }

    #[test]
    fn despawned_parent_is_treated_as_root_without_panic() {
        let mut world = World::new();
        let parent = spawn_node(&mut world, Vec3::new(10.0, 0.0, 0.0), None);
        let child = spawn_node(&mut world, Vec3::new(0.0, 5.0, 0.0), Some(parent));
        world.despawn(parent).expect("parent must despawn");

        run_propagation(&mut world);

        assert_eq!(global_translation(&world, child), Vec3::new(0.0, 5.0, 0.0));
    }

    #[test]
    fn parent_cycle_falls_back_to_local_transforms() {
        let mut world = World::new();
        let first = spawn_node(&mut world, Vec3::new(1.0, 0.0, 0.0), None);
        let second = spawn_node(&mut world, Vec3::new(0.0, 1.0, 0.0), Some(first));
        world
            .add_component(first, Parent(second))
            .expect("cycle setup must add Parent");
        let outsider = spawn_node(&mut world, Vec3::new(0.0, 0.0, 1.0), Some(second));

        run_propagation(&mut world);

        assert_eq!(global_translation(&world, first), Vec3::new(1.0, 0.0, 0.0));
        assert_eq!(global_translation(&world, second), Vec3::new(0.0, 1.0, 0.0));
        // A descendant outside the cycle still follows the fallback matrix.
        assert_eq!(
            global_translation(&world, outsider),
            Vec3::new(0.0, 1.0, 1.0)
        );
    }
}
