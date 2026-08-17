//! Mesh, material, and imported-texture loading with conversion diagnostics.

use super::*;

pub(super) fn load_mesh_asset(
    asset: &AssetId,
    asset_root: Option<&Path>,
    manifest: &AssetManifest,
    asset_state: &mut BridgeAssetState,
) -> (Mesh, Option<Diagnostic>) {
    match asset.as_str() {
        BUILTIN_TRIANGLE_ASSET_ID => {
            let conflict = builtin_conflict_diagnostic(asset, manifest);
            return (Mesh::triangle(), conflict);
        }
        BUILTIN_QUAD_ASSET_ID => {
            let conflict = builtin_conflict_diagnostic(asset, manifest);
            return (Mesh::quad(), conflict);
        }
        _ => {}
    }

    if let Some((source_id, entry, sub_asset)) = manifest.imported_sub_asset(asset) {
        if sub_asset.kind != ImportedSubAssetKind::Mesh {
            return (
                Mesh::cube(),
                Some(imported_asset_kind_diagnostic(
                    asset,
                    sub_asset.kind,
                    "mesh",
                )),
            );
        }
        let root = asset_root.unwrap_or_else(|| Path::new("."));
        let source_path = root.join(&entry.path);
        let existing_skeletons = &entry.import_settings.skeleton_records;
        let contact_bones = &entry.import_settings.contact_bones;
        return match import_gltf_cached(
            source_id,
            &source_path,
            existing_skeletons,
            contact_bones,
            asset_state,
        ) {
            Ok(imported) => imported
                .meshes
                .iter()
                .find(|mesh| &mesh.id == asset)
                .map(|mesh| (mesh.mesh.clone(), None))
                .unwrap_or_else(|| {
                    (
                        Mesh::cube(),
                        Some(imported_asset_missing_diagnostic(asset, &source_path)),
                    )
                }),
            Err(error) => (
                Mesh::cube(),
                Some(imported_asset_load_diagnostic(asset, &source_path, &error)),
            ),
        };
    }

    if let Some(entry) = manifest.get(asset) {
        let root = asset_root.unwrap_or_else(|| Path::new("."));
        let full_path = root.join(&entry.path);
        if crate::components::asset_path_matches_kind(
            crate::components::AssetKind::GltfSource,
            &full_path,
        ) {
            let existing_skeletons = &entry.import_settings.skeleton_records;
            let contact_bones = &entry.import_settings.contact_bones;
            return match import_gltf_cached(
                asset,
                &full_path,
                existing_skeletons,
                contact_bones,
                asset_state,
            ) {
                Ok(imported) => imported
                    .meshes
                    .first()
                    .map(|mesh| (mesh.mesh.clone(), None))
                    .unwrap_or_else(|| {
                        (
                            Mesh::cube(),
                            Some(imported_asset_missing_diagnostic(asset, &full_path)),
                        )
                    }),
                Err(error) => (
                    Mesh::cube(),
                    Some(imported_asset_load_diagnostic(asset, &full_path, &error)),
                ),
            };
        }
        match crate::asset::load_obj(&full_path) {
            Ok(mesh) => return (mesh, None),
            Err(source) => {
                let diag = Diagnostic::error(
                    "asset.missing_file",
                    format!("failed to load asset `{}`: {source}", asset.as_str()),
                )
                .with_target(DiagnosticTarget::Asset { id: asset.clone() });
                return (Mesh::cube(), Some(diag));
            }
        }
    }

    let diag = Diagnostic::warning(
        "asset.unregistered_file",
        format!(
            "asset `{}` is not registered in the manifest; using fallback mesh",
            asset.as_str()
        ),
    )
    .with_target(DiagnosticTarget::Asset { id: asset.clone() });
    (Mesh::cube(), Some(diag))
}

pub(super) fn load_material_asset(
    asset: &AssetId,
    asset_root: Option<&Path>,
    manifest: &AssetManifest,
    asset_state: &mut BridgeAssetState,
) -> (Material, Vec<Diagnostic>) {
    match asset.as_str() {
        BUILTIN_WHITE_MATERIAL_ASSET_ID => return (Material::default(), Vec::new()),
        BUILTIN_BLUE_MATERIAL_ASSET_ID => return (Material::color(0.2, 0.5, 1.0), Vec::new()),
        BUILTIN_ORANGE_MATERIAL_ASSET_ID => return (Material::color(0.9, 0.4, 0.1), Vec::new()),
        _ => {}
    }
    if let Some((source_id, entry, sub_asset)) = manifest.imported_sub_asset(asset) {
        if sub_asset.kind != ImportedSubAssetKind::Material {
            return (
                fallback_material(),
                vec![imported_asset_kind_diagnostic(
                    asset,
                    sub_asset.kind,
                    "material",
                )],
            );
        }
        // An extracted material (ADR 0101) resolves through the standalone
        // file it was extracted to, so every existing reference to this
        // sub-asset ID picks up edits without being reassigned.
        if let Some(remapped) = entry.import_settings.material_remaps.get(&sub_asset.id) {
            let remapped_id = AssetId::from_stable_id(engine_authoring::StableId::new(remapped));
            if let Ok(remapped_id) = remapped_id {
                return load_material_asset(&remapped_id, asset_root, manifest, asset_state);
            }
        }
        let root = asset_root.unwrap_or_else(|| Path::new("."));
        let source_path = root.join(&entry.path);
        let existing_skeletons = &entry.import_settings.skeleton_records;
        let contact_bones = &entry.import_settings.contact_bones;
        return match import_gltf_cached(
            source_id,
            &source_path,
            existing_skeletons,
            contact_bones,
            asset_state,
        ) {
            Ok(imported) => {
                let Some(imported_material) = imported
                    .materials
                    .iter()
                    .find(|material| &material.id == asset)
                else {
                    return (
                        fallback_material(),
                        vec![imported_asset_missing_diagnostic(asset, &source_path)],
                    );
                };
                let parsed = &imported_material.material;
                // Use the same resolver as standalone materials so imported
                // Texture IDs can honor model-level texture overrides too.
                let mut decode_slot =
                    |texture_id: &Option<AssetId>, slot: &'static str| {
                        texture_id
                            .as_ref()
                            .map(|texture_id| {
                                decode_material_texture(
                                    asset,
                                    texture_id,
                                    slot,
                                    asset_root,
                                    manifest,
                                    asset_state,
                                )
                            })
                            .transpose()
                    };
                let base = decode_slot(&parsed.base_color_texture, "base color");
                let normal = decode_slot(&parsed.normal_texture, "normal");
                let metallic_roughness =
                    decode_slot(&parsed.metallic_roughness_texture, "metallic/roughness");
                let occlusion = decode_slot(&parsed.occlusion_texture, "occlusion");
                let emissive = decode_slot(&parsed.emissive_texture, "emissive");
                let ramp = decode_slot(&parsed.toon.ramp_texture, "toon ramp");
                let sphere = decode_slot(&parsed.toon.sphere_texture, "sphere map");
                let mut diagnostics = imported
                    .diagnostics
                    .iter()
                    .cloned()
                    .map(|diagnostic| {
                        diagnostic.with_target(DiagnosticTarget::Asset { id: asset.clone() })
                    })
                    .collect::<Vec<_>>();
                match (
                    base,
                    normal,
                    metallic_roughness,
                    occlusion,
                    emissive,
                    ramp,
                    sphere,
                ) {
                    (
                        Ok(base),
                        Ok(normal),
                        Ok(metallic_roughness),
                        Ok(occlusion),
                        Ok(emissive),
                        Ok(ramp),
                        Ok(sphere),
                    ) => (
                        runtime_material_from_asset(
                            parsed,
                            crate::material::PendingMaterialTextures {
                                base,
                                normal,
                                metallic_roughness,
                                occlusion,
                                emissive,
                                ramp,
                                sphere,
                            },
                        ),
                        diagnostics,
                    ),
                    results => {
                        diagnostics.extend(
                            [
                                results.0.err(),
                                results.1.err(),
                                results.2.err(),
                                results.3.err(),
                                results.4.err(),
                                results.5.err(),
                                results.6.err(),
                            ]
                            .into_iter()
                            .flatten()
                            .map(|diagnostic| *diagnostic),
                        );
                        (fallback_material(), diagnostics)
                    }
                }
            }
            Err(error) => (
                fallback_material(),
                vec![imported_asset_load_diagnostic(asset, &source_path, &error)],
            ),
        };
    }
    let Some(entry) = manifest.get(asset) else {
        return (
            fallback_material(),
            vec![Diagnostic::warning(
                "asset.unregistered_file",
                format!(
                    "material `{}` is not registered; using diagnostic checker",
                    asset.as_str()
                ),
            )
            .with_target(DiagnosticTarget::Asset { id: asset.clone() })],
        );
    };
    let material_path = asset_root
        .unwrap_or_else(|| Path::new("."))
        .join(&entry.path);
    let parsed = std::fs::read_to_string(&material_path)
        .map_err(|error| error.to_string())
        .and_then(|json| {
            engine_authoring::MaterialAsset::from_json(&json).map_err(|error| error.to_string())
        });
    let parsed = match parsed {
        Ok(parsed) => parsed,
        Err(error) => {
            return (
                fallback_material(),
                vec![Diagnostic::error(
                    "asset.material_invalid",
                    format!(
                        "material `{}` failed to load from `{}`: {error}; using diagnostic checker",
                        asset.as_str(),
                        material_path.display()
                    ),
                )
                .with_target(DiagnosticTarget::Asset { id: asset.clone() })],
            )
        }
    };
    let mut decode_slot = |texture_id: &Option<AssetId>, slot: &'static str| {
        texture_id
            .as_ref()
            .map(|texture_id| {
                decode_material_texture(
                    asset,
                    texture_id,
                    slot,
                    asset_root,
                    manifest,
                    asset_state,
                )
            })
            .transpose()
    };
    let base = decode_slot(&parsed.base_color_texture, "base color");
    let normal = decode_slot(&parsed.normal_texture, "normal");
    let metallic_roughness =
        decode_slot(&parsed.metallic_roughness_texture, "metallic/roughness");
    let occlusion = decode_slot(&parsed.occlusion_texture, "occlusion");
    let emissive = decode_slot(&parsed.emissive_texture, "emissive");
    let ramp = decode_slot(&parsed.toon.ramp_texture, "toon ramp");
    let sphere = decode_slot(&parsed.toon.sphere_texture, "sphere map");
    match (
        base,
        normal,
        metallic_roughness,
        occlusion,
        emissive,
        ramp,
        sphere,
    ) {
        (
            Ok(base),
            Ok(normal),
            Ok(metallic_roughness),
            Ok(occlusion),
            Ok(emissive),
            Ok(ramp),
            Ok(sphere),
        ) => (
            runtime_material_from_asset(
                &parsed,
                crate::material::PendingMaterialTextures {
                    base,
                    normal,
                    metallic_roughness,
                    occlusion,
                    emissive,
                    ramp,
                    sphere,
                },
            ),
            Vec::new(),
        ),
        results => {
            let diagnostics = [
                results.0.err(),
                results.1.err(),
                results.2.err(),
                results.3.err(),
                results.4.err(),
                results.5.err(),
                results.6.err(),
            ]
                .into_iter()
                .flatten()
                .map(|diagnostic| *diagnostic)
                .collect();
            (fallback_material(), diagnostics)
        }
    }
}

fn decoded_imported_texture(
    texture_id: &Option<AssetId>,
    imported: &Arc<crate::model_import::GltfImportResult>,
    source_path: &Path,
    asset_state: &mut BridgeAssetState,
) -> Option<Arc<DecodedTexture>> {
    let texture_id = texture_id.as_ref()?;
    if let Some(texture) = asset_state.gltf_textures.get(texture_id) {
        return Some(Arc::clone(texture));
    }
    if let Some(shared) = &asset_state.shared_gltf_cache
        && let Some(texture) = shared.lookup_texture(texture_id, imported) {
            asset_state
                .gltf_textures
                .insert(texture_id.clone(), Arc::clone(&texture));
            return Some(texture);
        }
    let texture = imported
        .textures
        .iter()
        .find(|texture| &texture.id == texture_id)?;
    let decoded = Arc::new(DecodedTexture {
        label: format!("{} / {}", source_path.display(), texture.name),
        width: texture.width,
        height: texture.height,
        rgba8: texture.rgba8.clone(),
    });
    if let Some(shared) = &asset_state.shared_gltf_cache {
        shared.store_texture(texture_id, imported, &decoded);
    }
    asset_state
        .gltf_textures
        .insert(texture_id.clone(), Arc::clone(&decoded));
    Some(decoded)
}

/// Imports `source_path`, reusing a cached parse for `source_id` when one is
/// already available (conversion-local, then [`SharedGltfImportCache`] when
/// present).
///
/// `existing_skeletons` (ADR 0077) is normally this source's own persisted
/// `ImportSettings::skeleton_records`: recomputing the same file's bytes
/// reproduces the same [`crate::skeleton_asset::SkeletonIdentity`], so the
/// dedupe rule in `crate::model_import` adopts the same IDs the last editor
/// import resolved (including any cross-source adoption baked into that
/// record), without this call needing the whole project's ledger. A cache
/// hit skips re-resolution entirely and returns whatever `existing_skeletons`
/// produced the first time this source was imported in the current cache's
/// lifetime; this matches the existing accepted risk that a cached parse can
/// go stale if the file changes without a filesystem stamp change (ADR 0071).
///
/// `contact_bone_names` is normally this source's own persisted
/// `ImportSettings::contact_bones` (ADR 0080 §1, AP-5). Every call site for
/// one `source_id` MUST pass the same value within a conversion: the
/// conversion-local cache keys only on `source_id`, so a call that resolved
/// (and cached) a parse with one contact-bone override would otherwise hand
/// a later call for the same source a result built from a different
/// override.
pub(super) fn import_gltf_cached(
    source_id: &AssetId,
    source_path: &Path,
    existing_skeletons: &[crate::asset::SkeletonRecord],
    contact_bone_names: &[String],
    asset_state: &mut BridgeAssetState,
) -> Result<Arc<crate::model_import::GltfImportResult>, crate::model_import::ModelImportError> {
    if let Some(imported) = asset_state.gltf_imports.get(source_id) {
        return Ok(Arc::clone(imported));
    }
    if let Some(shared) = &asset_state.shared_gltf_cache
        && let Some(imported) = shared.lookup_source(
            source_id,
            source_path,
            existing_skeletons,
            contact_bone_names,
        )
    {
        asset_state
            .gltf_imports
            .insert(source_id.clone(), Arc::clone(&imported));
        return Ok(imported);
    }
    let imported = Arc::new(crate::model_import::import_model_path_with_contact_bones(
        source_id,
        source_path,
        existing_skeletons,
        contact_bone_names,
    )?);
    if let Some(shared) = &asset_state.shared_gltf_cache {
        shared.store_source(
            source_id,
            source_path,
            existing_skeletons,
            contact_bone_names,
            &imported,
        );
    }
    asset_state
        .gltf_imports
        .insert(source_id.clone(), Arc::clone(&imported));
    Ok(imported)
}

fn imported_asset_kind_diagnostic(
    asset: &AssetId,
    actual: ImportedSubAssetKind,
    expected: &str,
) -> Diagnostic {
    Diagnostic::error(
        "asset.imported_kind_mismatch",
        format!(
            "imported asset `{}` is {actual:?}, not a {expected}; using diagnostic fallback",
            asset.as_str()
        ),
    )
    .with_target(DiagnosticTarget::Asset { id: asset.clone() })
}

fn imported_asset_missing_diagnostic(asset: &AssetId, source_path: &Path) -> Diagnostic {
    Diagnostic::error(
        "asset.imported_sub_asset_missing",
        format!(
            "imported asset `{}` no longer exists in `{}`; reimport the source before continuing",
            asset.as_str(),
            source_path.display()
        ),
    )
    .with_target(DiagnosticTarget::Asset { id: asset.clone() })
}

fn imported_asset_load_diagnostic(
    asset: &AssetId,
    source_path: &Path,
    error: &crate::model_import::ModelImportError,
) -> Diagnostic {
    Diagnostic::error(
        "asset.imported_source_invalid",
        format!(
            "failed to load imported asset `{}` from `{}`: {error}; using diagnostic fallback",
            asset.as_str(),
            source_path.display()
        ),
    )
    .with_target(DiagnosticTarget::Asset { id: asset.clone() })
}

fn decode_material_texture(
    material_id: &AssetId,
    texture_id: &AssetId,
    slot: &'static str,
    asset_root: Option<&Path>,
    manifest: &AssetManifest,
    asset_state: &mut BridgeAssetState,
) -> Result<Arc<DecodedTexture>, Box<Diagnostic>> {
    let mut visited = std::collections::BTreeSet::new();
    decode_material_texture_inner(
        material_id,
        texture_id,
        slot,
        asset_root,
        manifest,
        asset_state,
        &mut visited,
    )
}

/// Resolves one material texture while rejecting malformed remap cycles.
fn decode_material_texture_inner(
    material_id: &AssetId,
    texture_id: &AssetId,
    slot: &'static str,
    asset_root: Option<&Path>,
    manifest: &AssetManifest,
    asset_state: &mut BridgeAssetState,
    visited: &mut std::collections::BTreeSet<AssetId>,
) -> Result<Arc<DecodedTexture>, Box<Diagnostic>> {
    if !visited.insert(texture_id.clone()) {
        return Err(Box::new(
            Diagnostic::error(
                "asset.material_texture_remap_cycle",
                format!(
                    "material `{}` has a cyclic {slot} texture override at `{}`; using diagnostic checker",
                    material_id.as_str(),
                    texture_id.as_str()
                ),
            )
            .with_target(DiagnosticTarget::Asset {
                id: material_id.clone(),
            }),
        ));
    }

    // Extracted materials retain Texture sub-asset IDs from their imported
    // source. Resolve those IDs through the owning model so extraction does
    // not turn valid embedded or sidecar images into unregistered assets.
    if let Some((source_id, source_entry, sub_asset)) = manifest.imported_sub_asset(texture_id) {
        if sub_asset.kind != ImportedSubAssetKind::Texture {
            return Err(Box::new(
                Diagnostic::error(
                    "asset.material_texture_kind_mismatch",
                    format!(
                        "material `{}` references {slot} asset `{}` of kind {:?}, not an imported texture; using diagnostic checker",
                        material_id.as_str(),
                        texture_id.as_str(),
                        sub_asset.kind
                    ),
                )
                .with_target(DiagnosticTarget::Asset {
                    id: material_id.clone(),
                }),
            ));
        }

        if let Some(remapped) = source_entry
            .import_settings
            .texture_remaps
            .get(&sub_asset.id)
        {
            let remapped_id = AssetId::from_stable_id(engine_authoring::StableId::new(remapped))
                .map_err(|error| {
                    Box::new(
                        Diagnostic::error(
                            "asset.material_texture_remap_invalid",
                            format!(
                                "material `{}` has an invalid {slot} texture override `{remapped}`: {error}; using diagnostic checker",
                                material_id.as_str()
                            ),
                        )
                        .with_target(DiagnosticTarget::Asset {
                            id: material_id.clone(),
                        }),
                    )
                })?;
            return decode_material_texture_inner(
                material_id,
                &remapped_id,
                slot,
                asset_root,
                manifest,
                asset_state,
                visited,
            );
        }

        let source_path = asset_root
            .unwrap_or_else(|| Path::new("."))
            .join(&source_entry.path);
        let imported = import_gltf_cached(
            source_id,
            &source_path,
            &source_entry.import_settings.skeleton_records,
            &source_entry.import_settings.contact_bones,
            asset_state,
        )
        .map_err(|error| {
            Box::new(
                Diagnostic::error(
                    "asset.material_texture_source_invalid",
                    format!(
                        "material `{}` could not load imported {slot} texture `{}` from `{}`: {error}; using diagnostic checker",
                        material_id.as_str(),
                        texture_id.as_str(),
                        source_path.display()
                    ),
                )
                .with_target(DiagnosticTarget::Asset {
                    id: material_id.clone(),
                }),
            )
        })?;

        return decoded_imported_texture(
            &Some(texture_id.clone()),
            &imported,
            &source_path,
            asset_state,
        )
        .ok_or_else(|| {
            Box::new(
                Diagnostic::error(
                    "asset.material_texture_import_missing",
                    format!(
                        "material `{}` references imported {slot} texture `{}` that no longer exists in `{}`; reimport the source before continuing",
                        material_id.as_str(),
                        texture_id.as_str(),
                        source_path.display()
                    ),
                )
                .with_target(DiagnosticTarget::Asset {
                    id: material_id.clone(),
                }),
            )
        });
    }

    let Some(texture_entry) = manifest.get(texture_id) else {
        return Err(Box::new(Diagnostic::error(
            "asset.material_texture_unregistered",
            format!(
                "material `{}` references unregistered {slot} texture `{}`; using diagnostic checker",
                material_id.as_str(),
                texture_id.as_str()
            ),
        )
        .with_target(DiagnosticTarget::Asset {
            id: material_id.clone(),
        })));
    };
    let texture_path = asset_root
        .unwrap_or_else(|| Path::new("."))
        .join(&texture_entry.path);
    let decoded = std::fs::read(&texture_path)
        .map_err(|error| error.to_string())
        .and_then(|bytes| {
            DecodedTexture::from_bytes(&bytes, texture_path.display().to_string())
                .map_err(|error| error.to_string())
        });
    decoded.map(Arc::new).map_err(|error| {
        Box::new(Diagnostic::error(
            "asset.material_texture_invalid",
            format!(
                "material `{}` {slot} texture `{}` failed to decode: {error}; using diagnostic checker",
                material_id.as_str(),
                texture_path.display()
            ),
        )
        .with_target(DiagnosticTarget::Asset {
            id: material_id.clone(),
        }))
    })
}

fn runtime_material_from_asset(
    asset: &engine_authoring::MaterialAsset,
    pending_textures: crate::material::PendingMaterialTextures,
) -> Material {
    Material::from_authoring_asset(asset).with_pending_texture_slots(pending_textures)
}

fn fallback_material() -> Material {
    let checker = DecodedTexture {
        label: "diagnostic_magenta_checker".into(),
        width: 2,
        height: 2,
        rgba8: vec![
            255, 0, 255, 255, 24, 24, 24, 255, 24, 24, 24, 255, 255, 0, 255, 255,
        ],
    };
    Material::pending_textured([1.0; 4], Arc::new(checker))
}

pub(super) fn builtin_conflict_diagnostic(
    asset: &AssetId,
    manifest: &AssetManifest,
) -> Option<Diagnostic> {
    if manifest.get(asset).is_some() {
        Some(
            Diagnostic::warning(
                "asset.builtin_conflict",
                format!(
                    "manifest redefines builtin asset `{}`; builtin takes precedence",
                    asset.as_str()
                ),
            )
            .with_target(DiagnosticTarget::Asset { id: asset.clone() }),
        )
    } else {
        None
    }
}
