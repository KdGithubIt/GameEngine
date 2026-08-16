# Architecture Decision Records

- [ADR 0052: Project Rust Gameplay Queries and Deferred Commands](0052-project-rust-gameplay-io.md)

Use this directory for decisions that change contracts shared across crates,
serialized project data, authoring commands, graph behavior, CLI, MCP, or the
visual editor.

ADR file names SHOULD use:

```text
NNNN-short-decision-name.md
```

Example:

```text
0001-canonical-authoring-format.md
```

Use this template:

```markdown
# ADR NNNN: Decision Title

Status: Proposed
Date: YYYY-MM-DD

## Context

What problem or ambiguity requires a decision?

## Decision

What will the project do?

## Consequences

What becomes easier, harder, required, or unsupported?

## Alternatives Considered

What other options were considered, and why were they not selected?

## Compatibility and Migration

Does this affect persisted data, public APIs, commands, diagnostics, or build
artifacts? If so, how will compatibility be maintained?
```

Valid status values are:

- Proposed
- Accepted
- Superseded
- Rejected

Accepted ADRs are part of the implementation contract. An ADR that changes the
canonical specification MUST also update
`docs/AI_FRIENDLY_AUTHORING_SPEC.md`.

## Accepted Decisions

| ADR | Decision |
| --- | --- |
| [0001](0001-runtime-ecs-access-and-safety-model.md) | Runtime ECS Access and Safety Model |
| [0002](0002-runtime-and-authoring-identifier-boundary.md) | Runtime and Authoring Identifier Boundary |
| [0003](0003-runtime-renderer-ownership-boundary.md) | Runtime Renderer Ownership Boundary |
| [0004](0004-stable-identifier-format.md) | Stable Identifier Format |
| [0005](0005-authoring-transaction-and-undo-strategy.md) | Authoring Transaction and Undo Strategy |
| [0006](0006-graph-foundation-placement.md) | Graph Foundation Placement |
| [0007](0007-graph-document-and-transaction-boundary.md) | Graph Document and Transaction Boundary |
| [0008](0008-graph-semantic-and-view-serialization.md) | Graph Semantic and View Serialization |
| [0009](0009-graph-schema-and-port-type-compatibility.md) | Graph Schema and Port Type Compatibility |
| [0010](0010-graphview-document-and-presentation-transaction-boundary.md) | GraphView Document and Presentation Transaction Boundary |
| [0011](0011-graph-domain-validation-boundary.md) | Graph Domain Validation Boundary |
| [0012](0012-phase4-first-domain-behavior-tree.md) | Phase 4 First Domain Behavior Tree |
| [0013](0013-graph-domain-compiled-representation-contract.md) | Graph Domain Compiled Representation Contract |
| [0014](0014-behavior-tree-runtime-executor.md) | Behavior Tree Runtime Executor |
| [0015](0015-mcp-behavior-tree-tool-adapter.md) | MCP Behavior Tree Tool Adapter |
| [0016](0016-human-visual-editor-command-boundary.md) | Human Visual Editor Command Boundary |
| [0017](0017-phase8a-human-editor-gui-toolkit.md) | Phase 8-A Human Editor GUI Toolkit |
| [0018](0018-phase8b-undo-redo-snapshot-strategy.md) | Phase 8-B Undo/Redo Snapshot Strategy |
| [0019](0019-phase8c-editor-file-persistence.md) | Phase 8-C Editor File Persistence (superseded by ADR 0022) |
| [0020](0020-scene-document-schema-version.md) | Scene Document Schema Version |
| [0021](0021-asset-reference-model-and-manifest.md) | Asset Reference Model and Asset Manifest |
| [0022](0022-project-document-file-layout.md) | Project Document File Layout (Return to ADR 0008) |
| [0023](0023-project-root-ownership.md) | Project Root Ownership in engine-authoring |
| [0024](0024-editor-play-process-model.md) | Editor Play Process Model and GPU Stack Unification |
| [0025](0025-editor-component-inspector-staging.md) | Editor Component Inspector Staging |
| [0026](0026-virtual-input-and-ai-observation-boundary.md) | Virtual Input and AI Observation Boundary |
| [0027](0027-component-definition-registry.md) | Component Definition and Registry Boundary |
| [0028](0028-advanced-authoring-roadmap-boundaries.md) | Advanced Authoring Roadmap Boundaries |
| [0029](0029-asset-manifest-v2-import-settings.md) | Asset Manifest v2 — Import Settings Extension |
| [0030](0030-prefab-schema-v1.md) | Prefab Schema v1 |
| [0031](0031-project-settings-schema.md) | Project Settings Schema |
| [0032](0032-gltf-static-mesh-import.md) | glTF / GLB Static Mesh Import Pipeline |
| [0033](0033-animation-graph-schema.md) | Animation Graph Schema and Compile Contract |
| [0034](0034-build-packaging-strategy.md) | Build Packaging Strategy |
| [0035](0035-ai-agent-bridge-ipc.md) | AI Agent Bridge IPC and MCP Adapter |
| [0036](0036-shadow-map-and-environment-lighting.md) | Shadow Map and Environment Lighting Contract (Phase 41) |
| [0037](0037-rhai-scripting-runtime.md) | Rhai Scripting Runtime (Phase 42) |
| [0038](0038-gamepad-input-and-gilrs.md) | Gamepad Input and gilrs Desktop Backend (Phase 43) |
| [0039](0039-navmesh-and-pathfinding-mvp.md) | NavMesh and Pathfinding MVP (Phase 44) |
| [0040](0040-post-processing-pipeline.md) | Post-Processing Pipeline and HDR Target (Phase 45) |
| [0041](0041-wasm32-build-strategy.md) | wasm32 Build Strategy (Phase 46) |
| [0042](0042-gpu-instancing-and-lod.md) | GPU Instancing and Level-of-Detail (Phase 47) |
| [0043](0043-skinned-mesh-skeletal-animation.md) | Skinned Mesh & Skeletal Animation (Phase 48) |
| [0044](0044-particle-system.md) | Particle System — CPU Simulation + Instanced Rendering (Phase 49) |
| [0045](0045-player-binary-and-package-execution.md) | Player Binary and Package Execution (Phase 51) |
| [0046](0046-declarative-ui-document.md) | Declarative UI Document Model and egui Interpreter |
| [0047](0047-runtime-scene-management.md) | Runtime Scene Management and Switch Semantics |
| [0048](0048-save-data-format-and-storage.md) | Save Data Format and Storage |
| [0049](0049-script-api-v2-command-boundary.md) | Script API v2 Command Boundary |
| [0050](0050-native-rust-game-modules.md) | Native Rust Game Module Boundary |
| [0051](0051-ecs-system-ordering-and-project-settings.md) | ECS System Identity, Ordering, and Project Settings |
| [0052](0052-project-rust-gameplay-io.md) | Project Rust Gameplay Queries and Deferred Commands |
| [0053](0053-input-actions-and-character-motor.md) | Input Actions and Character Motor Contract |
| [0054](0054-authorable-runtime-component-contract.md) | Authorable Runtime Component and Inspector Contract |
| [0055](0055-render-authoring-and-material-v2-contract.md) | Render Authoring and Material v2 Contract |
| [0056](0056-editor-ready-collision-and-combat-contract.md) | Editor Ready Collision, Character, and Combat Contract |
| [0057](0057-editor-ready-navigation-and-behavior-contract.md) | Editor Ready Navigation and Behavior Contract |
| [0058](0058-distribution-debugging-and-proving-project-contract.md) | Distribution, Debugging, and Proving Project Contract |
| [0059](0059-transform-v2-and-editor-manipulation-contract.md) | Transform v2 and Editor Manipulation Contract |
| [0060](0060-editor-ready-ui-and-workflow-documents.md) | Editor-ready UI and Workflow Documents |
| [0061](0061-component-source-discovery-and-sdk-source-bundle.md) | Component Source Discovery and SDK Source Bundle |
| [0062](0062-asset-folder-batch-transaction.md) | Transactional Asset Folder Moves |
| [0063](0063-responsive-ui-document-schema.md) | Responsive UI Schema v3 |
| [0064](0064-deterministic-input-replay-format.md) | Deterministic Input Replay Format |
| [0065](0065-typed-project-gameplay-api.md) | Typed Project Gameplay API |
| [0066](0066-project-component-sidecar-metadata.md) | Project Component Sidecar Metadata |
| [0067](0067-unified-script-assets-and-generated-cargo-host.md) | Unified Script Assets and Generated Cargo Host |
| [0068](0068-best-effort-scene-view-conversion.md) | Best-Effort Scene View Conversion |
| [0069](0069-inactive-components-for-unassigned-references.md) | Inactive Components for Unassigned Asset References |
| [0070](0070-entity-enabled-flag.md) | Entity Enabled Flag |
| [0071](0071-shared-gltf-import-cache.md) | Shared glTF Import Cache for Repeated Conversions |
| [0072](0072-persistent-scene-view-preview-world.md) | Persistent Scene View Preview World |
| [0073](0073-shared-ui-interpreter-for-builder-preview.md) | Shared UI Interpreter for the Builder Preview |
| [0074](0074-model-import-instantiation-and-shared-skeletons.md) | Model Import Instantiation and Shared Skeletons |
| [0075](0075-automatic-model-import-and-hidden-artifacts.md) | Automatic Model Import and Hidden Import Artifacts |
| [0076](0076-submeshes-and-material-slots.md) | Submeshes and Material Slots |
| [0077](0077-skeleton-assets-and-stable-bone-ids.md) | Skeleton Assets, Stable Bone IDs, and Clip Binding |
| [0078](0078-format-independent-model-ir.md) | Format-Independent Model Intermediate Representation |
| [0079](0079-retarget-maps-and-derived-clip-cache.md) | Retarget Maps and the Derived Clip Cache |
| [0080](0080-contact-metadata-and-runtime-foot-ik.md) | Contact Metadata and Runtime Foot IK |
| [0081](0081-fbx-import-via-ufbx.md) | FBX Import via ufbx |
| [0082](0082-unified-animation-controller-authoring.md) | Unified Animation Controller Authoring |
| [0083](0083-unified-mesh-renderer-authoring.md) | Unified Mesh Renderer Authoring |
| [0084](0084-required-animation-graph-authoring.md) | Required Animation Graph Authoring |
| [0085](0085-animation-sets-and-motion-slots.md) | Animation Sets and Stable Motion Slots |
| [0086](0086-runtime-rig-pose-and-skin-binding.md) | Runtime Rig Pose and Per-Mesh Skin Binding |
| [0087](0087-skinned-model-and-render-parts.md) | Skinned Model, Render Parts, and Rig Ownership |
| [0088](0088-bone-attachment-and-rig-queries.md) | Bone Attachment and Reading a Rig's Pose |
| [0089](0089-skinned-model-bind-pose-bake.md) | Skinned Model Bind-Pose Bake to Static Mesh |
| [0090](0090-ui-layout-screen-versus-presented-rect.md) | UI Layout Screen Separated from the Presented Rectangle |
| [0091](0091-remove-authoring-compatibility-surface.md) | Remove the Authoring Compatibility Surface |
| [0092](0092-unified-project-rust-module-tree.md) | Unified Project Rust Module Tree |
| [0096](0096-rapier-physics-adoption-and-mmd-rigid-body-bridge.md) | Rapier Physics Adoption and the MMD Rigid-Body Bridge Boundary |
| [0097](0097-mmd-pmx-vmd-import.md) | MMD (PMX/VMD) Import — Mesh, Baked Animation, Morphs, and Skin Splitting |
| [0098](0098-vmd-motion-composition-and-content-routing.md) | VMD Motion Composition and Content Routing |
| [0099](0099-vmd-multi-target-derived-clips.md) | VMD Multi-Target Derived Clips |
| [0100](0100-generic-shading-models-and-outline-pass.md) | Generic Shading Models and Outline Pass |
| [0101](0101-imported-material-extraction-and-remap.md) | Imported Material Extraction and Remap |
| [0103](0103-play-mode-editor-viewport.md) | Play Mode Editor Viewport |
| [0104](0104-editor-edit-responsiveness.md) | Editor Edit Responsiveness |
| [0106](0106-layered-rig-pose-and-pose-graph-boundary.md) | Layered Rig Pose and Pose Graph Boundary |
| [0107](0107-active-game-camera-selection.md) | Active Game Camera Selection |
| [0108](0108-mmd-secondary-motion-simulation-units.md) | MMD Secondary-Motion Constraint and Unit Semantics (superseded by ADR 0112) |
| [0109](0109-mmd-seek-physics-preroll.md) | MMD Seek Physics Pre-Roll (superseded by ADR 0112) |
| [0110](0110-humanoid-profiles-and-skeleton-independent-motion.md) | Humanoid Profiles and Skeleton-Independent Motion |
| [0111](0111-runtime-rig-crate-boundary.md) | Runtime Rig Crate Boundary |
| [0112](0112-engine-native-secondary-motion-and-pmx-physics-conversion.md) | Engine-Native Secondary Motion and Best-Effort PMX Physics Conversion |
| [0113](0113-runtime-domain-crate-decomposition.md) | Runtime Domain Crate Decomposition |
| [0114](0114-heavy-third-party-dependency-isolation.md) | Heavy Third-Party Dependency Isolation |
| [0115](0115-current-format-only-baseline.md) | Current-Format-Only Baseline |
| [0116](0116-animation-events-owned-by-animation-set-bindings.md) | Animation Events Owned by Animation Set Bindings |
| [0117](0117-project-first-launcher-and-editor-application-lifecycle.md) | Project-First Launcher and Editor Application Lifecycle |
| [0118](0118-rendering-color-space-and-texture-semantics.md) | Rendering Color-Space and Texture-Semantics Contract |
| [0119](0119-tangent-space-gpu-contract-and-shared-material-fragment.md) | Tangent-Space GPU Contract and Shared Material Fragment Stage |
| [0120](0120-generic-pbr-material-texture-contract.md) | Generic PBR Material Texture Contract |
| [0121](0121-ai-native-authoring-parity-and-project-scoped-mcp-lifecycle.md) | AI-Native Authoring Parity and Project-Scoped MCP Lifecycle |
| [0122](0122-renderer-owned-full-image-based-lighting.md) | Renderer-Owned Full Image-Based Lighting |

## Phase 41+ ADR Targets

| ADR | Target Phase | Decision Area | Status |
| --- | --- | --- | --- |
| [0036](0036-shadow-map-and-environment-lighting.md) | Phase 41 | Shadow map format, resolution, and cascade policy | Accepted |
| [0037](0037-rhai-scripting-runtime.md) | Phase 42 | Rhai scripting runtime and sandbox boundary | Accepted |
| [0038](0038-gamepad-input-and-gilrs.md) | Phase 43 | Gamepad / `gilrs` integration and platform build gates | Accepted |
| [0039](0039-navmesh-and-pathfinding-mvp.md) | Phase 44 | NavMesh library selection and bake strategy | Accepted |
| [0040](0040-post-processing-pipeline.md) | Phase 45 | Post-processing pipeline and HDR render target strategy | Accepted |
| [0041](0041-wasm32-build-strategy.md) | Phase 46 | WASM / web build target strategy | Accepted |
| [0042](0042-gpu-instancing-and-lod.md) | Phase 47 | GPU instancing and LOD selection | Accepted |
| [0043](0043-skinned-mesh-skeletal-animation.md) | Phase 48 | Skinned mesh vertex layout, skeleton representation, joint palette, clip targets | Accepted |
| [0044](0044-particle-system.md) | Phase 49 | Particle simulation ownership, rendering path, RNG policy | Accepted |
| [0045](0045-player-binary-and-package-execution.md) | Phase 51 | Player binary strategy, package layout, blocking policy | Accepted |
