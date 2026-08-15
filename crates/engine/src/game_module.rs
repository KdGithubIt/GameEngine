//! Versioned native Rust game-module discovery, loading, and dispatch.
//!
//! Project libraries expose only the C-compatible descriptor defined here.
//! Component and query-scoped gameplay values are transferred as bounded JSON
//! bytes; ECS values never cross the dynamic-library boundary by value. See
//! ADRs 0050 and 0052.

use crate::game_commands::{apply_prepared_game_commands, prepare_game_commands, GameCommandError};
use crate::game_host::{
    apply_game_output, compile_game_invocation, GameHostApplyError, GameHostCompileError,
    GameHostRuntime,
};
use crate::game_io::{
    validate_game_input_bytes, validate_game_output_bytes, GameAccessError, GameInvocation,
    GameInvocationOutput, GameIoLimitError,
};
use engine_ecs::{Entity, SystemDescriptor, SystemId, SystemOrigin, World};
use std::collections::{BTreeMap, BTreeSet};
use std::ffi::{c_char, CStr};
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

pub use engine_authoring::id::ComponentTypeId;
pub use engine_authoring::schema::{ComponentSchema, FieldSchema, FieldType};
pub use engine_authoring::value::Value;

const ERROR_BUFFER_SIZE: usize = 4096;

pub use engine_scripting::game_contracts::{
    GameComponent, GameField, GameResource, GameResourceSchema, GameSystemSchedule,
};
pub use engine_scripting::game_module::*;
use engine_scripting::game_contracts::{validate_resource_schema, validate_resource_value};

/// Engine-owned ECS component containing all project component authoring data.
#[derive(Debug, Clone, Default)]
pub struct GameComponentStore {
    components: BTreeMap<ComponentTypeId, Value>,
    disabled: BTreeSet<ComponentTypeId>,
}

impl GameComponentStore {
    /// Returns one project component's current authoring-shaped runtime value.
    pub fn value(&self, component_type: &ComponentTypeId) -> Option<&Value> {
        (!self.disabled.contains(component_type))
            .then(|| self.components.get(component_type))
            .flatten()
    }

    pub(crate) fn is_enabled(&self, component_type: &ComponentTypeId) -> Option<bool> {
        self.components
            .contains_key(component_type)
            .then(|| !self.disabled.contains(component_type))
    }

    pub(crate) fn remove_runtime_value(&mut self, component_type: &ComponentTypeId) {
        self.components.remove(component_type);
        self.disabled.remove(component_type);
    }

    pub(crate) fn set_enabled(&mut self, component_type: &ComponentTypeId, enabled: bool) {
        assert!(
            self.components.contains_key(component_type),
            "component existence must be checked during atomic preflight"
        );
        if enabled {
            self.disabled.remove(component_type);
        } else {
            self.disabled.insert(component_type.clone());
        }
    }

    pub(crate) fn insert_runtime_value(&mut self, component_type: ComponentTypeId, value: Value) {
        self.disabled.remove(&component_type);
        self.components.insert(component_type, value);
    }
}

/// Host-copied defaults for project components in the active module generation.
#[derive(Debug, Clone, Default)]
pub(crate) struct GameComponentDefaults {
    values: BTreeMap<ComponentTypeId, Value>,
}

impl GameComponentDefaults {
    pub(crate) fn from_module(module: &GameModule) -> Self {
        Self {
            values: module
                .component_schemas()
                .map(|schema| (schema.type_id.clone(), schema.default_value()))
                .collect(),
        }
    }

    pub(crate) fn get(&self, component_type: &ComponentTypeId) -> Option<&Value> {
        self.values.get(component_type)
    }

    #[cfg(test)]
    pub(crate) fn insert(&mut self, component_type: ComponentTypeId, value: Value) {
        self.values.insert(component_type, value);
    }
}

/// Defines the single exported entry point in a project game library.
#[macro_export]
macro_rules! export_game_module {
    () => {
        /// Returns this project's versioned native game-module descriptor.
        #[unsafe(no_mangle)]
        pub extern "C" fn iroha_game_module_v3(
        ) -> *const engine::game_module::GameModuleDescriptorAbi {
            engine::game_module::exported_descriptor()
        }
    };
}

#[derive(Clone)]
struct LoadedComponent {
    schema: ComponentSchema,
    validate: GameComponentValidateAbi,
}

#[derive(Clone)]
struct LoadedResource {
    schema: GameResourceSchema,
}
#[derive(Clone)]
struct LoadedSystem {
    metadata: GameSystemMetadata,
    descriptor: SystemDescriptor,
    run: GameSystemRunAbi,
}

struct GameCallbackOutput {
    output: GameInvocationOutput,
    callback_time: Duration,
    input_bytes: usize,
    output_bytes: usize,
}

/// Loaded native game library retained for the lifetime of its ECS values.
pub struct GameModule {
    path: PathBuf,
    components: BTreeMap<ComponentTypeId, LoadedComponent>,
    resources: BTreeMap<String, LoadedResource>,
    systems: Vec<LoadedSystem>,
    free_buffer: unsafe extern "C" fn(GameBufferAbi),
    #[cfg(not(target_arch = "wasm32"))]
    _library: libloading::Library,
}

impl fmt::Debug for GameModule {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("GameModule")
            .field("path", &self.path)
            .field("component_count", &self.components.len())
            .field("resource_count", &self.resources.len())
            .field("system_count", &self.systems.len())
            .finish()
    }
}

impl GameModule {
    /// Loads and validates a desktop native game library.
    #[cfg(not(target_arch = "wasm32"))]
    pub fn load(path: &Path) -> Result<Self, GameModuleLoadError> {
        type Entry = unsafe extern "C" fn() -> *const GameModuleDescriptorAbi;
        // SAFETY: The handle is retained beyond all copied callback addresses.
        let library = unsafe { libloading::Library::new(path) }.map_err(|source| {
            GameModuleLoadError::Open {
                path: path.to_path_buf(),
                source,
            }
        })?;
        // SAFETY: This symbol name and signature are fixed by ADR 0052.
        let entry: libloading::Symbol<'_, Entry> =
            match unsafe { library.get(b"iroha_game_module_v3\0") } {
                Ok(entry) => entry,
                Err(missing) => return Err(GameModuleLoadError::MissingEntry(missing)),
            };
        // SAFETY: The entry takes no arguments and returns immutable storage.
        let descriptor = unsafe { entry() };
        if descriptor.is_null() {
            return Err(GameModuleLoadError::NullDescriptor);
        }
        // SAFETY: The library retains the non-null descriptor.
        let descriptor = unsafe { &*descriptor };
        if descriptor.abi_version != GAME_MODULE_ABI_VERSION {
            return Err(GameModuleLoadError::AbiMismatch {
                expected: GAME_MODULE_ABI_VERSION,
                found: descriptor.abi_version,
            });
        }
        let fingerprint = read_ffi_string(descriptor.sdk_fingerprint, "SDK fingerprint")?;
        let expected = CStr::from_bytes_with_nul(GAME_MODULE_SDK_FINGERPRINT)
            .expect("fingerprint has NUL")
            .to_string_lossy();
        if fingerprint != expected {
            return Err(GameModuleLoadError::SdkMismatch {
                expected: expected.into_owned(),
                found: fingerprint,
            });
        }
        let component_descriptors = read_ffi_slice(
            descriptor.components,
            descriptor.component_count,
            "component descriptors",
        )?;
        let mut components = BTreeMap::new();
        for component in component_descriptors {
            let type_id_text = read_ffi_string(component.type_id, "component type ID")?;
            let type_id = ComponentTypeId::try_new(type_id_text.clone()).map_err(|source| {
                GameModuleLoadError::InvalidComponentId {
                    value: type_id_text,
                    source,
                }
            })?;
            let schema_json = read_ffi_string(component.schema_json, "component schema")?;
            let schema: ComponentSchema = serde_json::from_str(&schema_json).map_err(|source| {
                GameModuleLoadError::InvalidSchema {
                    component_type: type_id.clone(),
                    source,
                }
            })?;
            if schema.type_id != type_id {
                return Err(GameModuleLoadError::SchemaIdMismatch {
                    descriptor: type_id,
                    schema: schema.type_id,
                });
            }
            if components
                .insert(
                    schema.type_id.clone(),
                    LoadedComponent {
                        schema,
                        validate: component.validate,
                    },
                )
                .is_some()
            {
                return Err(GameModuleLoadError::DuplicateComponent(type_id));
            }
        }
        let system_descriptors = read_ffi_slice(
            descriptor.systems,
            descriptor.system_count,
            "system descriptors",
        )?;
        // Decode every record before validating access because the resource
        // registry is module-level metadata carried by one deterministic
        // record, not necessarily the record currently being validated.
        let mut decoded_systems = Vec::new();
        for system in system_descriptors {
            let metadata_json = read_ffi_string(system.metadata_json, "system metadata")?;
            let metadata: GameSystemMetadataEnvelope = serde_json::from_str(&metadata_json)
                .map_err(|source| GameModuleLoadError::InvalidSystemMetadata { source })?;
            decoded_systems.push((metadata, system.run));
        }
        let mut resources = BTreeMap::new();
        for (envelope, _) in &decoded_systems {
            for schema in &envelope.module_resources {
                validate_resource_schema(schema).map_err(|message| {
                    GameModuleLoadError::InvalidResourceSchema {
                        resource_id: schema.id.clone(),
                        message,
                    }
                })?;
                if resources
                    .insert(
                        schema.id.clone(),
                        LoadedResource {
                            schema: schema.clone(),
                        },
                    )
                    .is_some()
                {
                    return Err(GameModuleLoadError::DuplicateResource(schema.id.clone()));
                }
            }
        }

        let mut ids = BTreeSet::new();
        let mut systems = Vec::new();
        for (envelope, run) in decoded_systems {
            let metadata = envelope.metadata;
            metadata.access.validate().map_err(|source| {
                GameModuleLoadError::InvalidSystemAccess {
                    system_id: metadata.id.clone(),
                    source,
                }
            })?;
            for component_type in metadata
                .access
                .queries
                .iter()
                .flat_map(|query| query.components.iter())
                .map(|component| &component.component_type)
            {
                if !components.contains_key(component_type) {
                    return Err(GameModuleLoadError::UnknownAccessComponent {
                        system_id: metadata.id.clone(),
                        component_type: component_type.clone(),
                    });
                }
            }
            for resource in &metadata.access.resources {
                if !resources.contains_key(&resource.id) {
                    return Err(GameModuleLoadError::UnknownAccessResource {
                        system_id: metadata.id.clone(),
                        resource_id: resource.id.clone(),
                    });
                }
            }
            let canonical_id = SystemId::try_new(metadata.id.clone()).map_err(|source| {
                GameModuleLoadError::InvalidSystemId {
                    value: metadata.id.clone(),
                    source,
                }
            })?;
            if !ids.insert(canonical_id.clone()) {
                return Err(GameModuleLoadError::DuplicateSystem(metadata.id));
            }
            for alias in &metadata.aliases {
                let alias = SystemId::try_new(alias.clone()).map_err(|source| {
                    GameModuleLoadError::InvalidSystemId {
                        value: alias.clone(),
                        source,
                    }
                })?;
                if !ids.insert(alias.clone()) {
                    return Err(GameModuleLoadError::DuplicateSystem(alias.to_string()));
                }
            }
            let mut descriptor = SystemDescriptor::new(
                metadata.id.clone(),
                metadata.display_name.clone(),
                SystemOrigin::Game,
            )
            .map_err(|source| GameModuleLoadError::InvalidSystemId {
                value: metadata.id.clone(),
                source,
            })?
            .with_description(metadata.description.clone());
            for target in &metadata.before {
                descriptor = descriptor.try_before(target.clone()).map_err(|source| {
                    GameModuleLoadError::InvalidSystemId {
                        value: target.clone(),
                        source,
                    }
                })?;
            }
            for target in &metadata.after {
                descriptor = descriptor.try_after(target.clone()).map_err(|source| {
                    GameModuleLoadError::InvalidSystemId {
                        value: target.clone(),
                        source,
                    }
                })?;
            }
            for alias in &metadata.aliases {
                descriptor = descriptor.try_alias(alias.clone()).map_err(|source| {
                    GameModuleLoadError::InvalidSystemId {
                        value: alias.clone(),
                        source,
                    }
                })?;
            }
            systems.push(LoadedSystem {
                metadata,
                descriptor,
                run,
            });
        }
        systems.sort_by(|left, right| {
            (
                left.metadata.schedule,
                left.metadata.order,
                left.metadata.id.as_str(),
            )
                .cmp(&(
                    right.metadata.schedule,
                    right.metadata.order,
                    right.metadata.id.as_str(),
                ))
        });
        Ok(Self {
            path: path.to_path_buf(),
            components,
            resources,
            systems,
            free_buffer: descriptor.free_buffer,
            _library: library,
        })
    }

    /// Returns the loaded shadow-copy path.
    pub fn path(&self) -> &Path {
        &self.path
    }
    /// Iterates project component schemas in stable type-ID order.
    pub fn component_schemas(&self) -> impl Iterator<Item = &ComponentSchema> {
        self.components.values().map(|component| &component.schema)
    }
    /// Returns the schema for one project component type.
    pub fn component_schema(&self, component_type: &ComponentTypeId) -> Option<&ComponentSchema> {
        self.components
            .get(component_type)
            .map(|component| &component.schema)
    }
    /// Iterates project resource schemas in stable resource-ID order.
    pub fn resource_schemas(&self) -> impl Iterator<Item = &GameResourceSchema> {
        self.resources.values().map(|resource| &resource.schema)
    }
    /// Returns the schema for one project resource ID.
    pub fn resource_schema(&self, resource_id: &str) -> Option<&GameResourceSchema> {
        self.resources
            .get(resource_id)
            .map(|resource| &resource.schema)
    }
    /// Iterates loaded project system metadata in default schedule order.
    pub fn system_metadata(&self) -> impl Iterator<Item = &GameSystemMetadata> {
        self.systems.iter().map(|system| &system.metadata)
    }

    pub(crate) fn system_entries(
        &self,
    ) -> impl Iterator<Item = (usize, &GameSystemMetadata, &SystemDescriptor)> {
        self.systems
            .iter()
            .enumerate()
            .map(|(index, system)| (index, &system.metadata, &system.descriptor))
    }
    /// Adds a project component from authoring data to a runtime entity.
    pub fn spawn_component(
        &self,
        world: &mut World,
        entity: Entity,
        component_type: &ComponentTypeId,
        value: &Value,
    ) -> Result<(), GameModuleRunError> {
        let component = self
            .components
            .get(component_type)
            .ok_or_else(|| GameModuleRunError::MissingComponent(component_type.clone()))?;
        let value_json =
            serde_json::to_vec(value).map_err(|source| GameModuleRunError::SerializeComponent {
                component_type: component_type.clone(),
                source,
            })?;
        let mut error = [0_u8; ERROR_BUFFER_SIZE];
        // SAFETY: Load validated the ABI and both byte buffers satisfy the
        // callback contract. No Rust or ECS layout crosses the boundary.
        let success = unsafe {
            (component.validate)(
                value_json.as_ptr(),
                value_json.len(),
                error.as_mut_ptr(),
                error.len(),
            )
        };
        if !success {
            return Err(GameModuleRunError::ComponentSpawn {
                component_type: component_type.clone(),
                message: ffi_error_message(&error),
            });
        }
        if let Some(store) = world.get_component_mut::<GameComponentStore>(entity) {
            store
                .components
                .insert(component_type.clone(), value.clone());
            Ok(())
        } else {
            let mut store = GameComponentStore::default();
            store
                .components
                .insert(component_type.clone(), value.clone());
            world
                .add_component(entity, store)
                .map_err(|source| GameModuleRunError::ComponentStore { source })
        }
    }
    /// Runs every project system registered for `schedule`.
    pub fn run_systems(
        &self,
        schedule: GameSystemSchedule,
        world: &mut World,
    ) -> Result<(), GameModuleRunError> {
        let indices: Vec<_> = self
            .systems
            .iter()
            .enumerate()
            .filter_map(|(index, system)| (system.metadata.schedule == schedule).then_some(index))
            .collect();
        for index in indices {
            self.run_system_scoped(index, world)?;
        }
        Ok(())
    }

    /// Compiles and runs one ABI v3 callback against its declared access only.
    ///
    /// The host runtime resource is temporarily removed because applying an
    /// output needs simultaneous exclusive access to the ECS world and the
    /// host-owned game-resource map. It is reinserted on every success and
    /// failure path before the result is returned.
    pub(crate) fn run_system_scoped(
        &self,
        index: usize,
        world: &mut World,
    ) -> Result<(), GameModuleRunError> {
        let system = self
            .systems
            .get(index)
            .ok_or(GameModuleRunError::UnknownSystemIndex(index))?;
        let mut runtime = world
            .remove_resource::<GameHostRuntime>()
            .unwrap_or_default();
        let result = (|| {
            runtime.refresh_for_system(world, &system.metadata.id, &system.metadata.access);
            let invocation = compile_game_invocation(
                world,
                &system.metadata.id,
                &system.metadata.access,
                runtime.frame(),
            )
            .map_err(|source| GameModuleRunError::InvocationCompile {
                system_id: system.metadata.id.clone(),
                source,
            })?;
            let query_rows = invocation
                .queries
                .iter()
                .map(|query| query.rows.len())
                .sum();
            let callback = self.run_callback(system, &invocation)?;
            self.validate_component_patch_values(&system.metadata.id, &callback.output)?;
            self.validate_resource_patch_values(&system.metadata.id, &callback.output)?;
            let command_count = callback.output.commands.len();
            let prepared_commands = prepare_game_commands(world, &callback.output.commands)
                .map_err(|source| GameModuleRunError::CommandPreparation {
                    system_id: system.metadata.id.clone(),
                    source,
                })?;
            let mut effects = apply_game_output(
                world,
                &system.metadata.access,
                &invocation,
                &mut runtime.frame_mut().resources,
                callback.output,
            )
            .map_err(|source| GameModuleRunError::OutputApply {
                system_id: system.metadata.id.clone(),
                source,
            })?;
            apply_prepared_game_commands(world, prepared_commands);
            effects.commands.clear();
            Ok((
                effects,
                callback.callback_time,
                callback.input_bytes,
                callback.output_bytes,
                query_rows,
                command_count,
            ))
        })();
        let result = match result {
            Ok((effects, callback_time, input_bytes, output_bytes, query_rows, command_count)) => {
                runtime.accept_effects(&system.metadata.id, effects);
                runtime.record_success(
                    &system.metadata.id,
                    callback_time,
                    input_bytes,
                    output_bytes,
                    query_rows,
                    command_count,
                );
                Ok(())
            }
            Err(error) => {
                runtime.record_failure(&system.metadata.id, &error);
                Err(error)
            }
        };
        world.insert_resource(runtime);
        result
    }

    fn validate_component_patch_values(
        &self,
        system_id: &str,
        output: &GameInvocationOutput,
    ) -> Result<(), GameModuleRunError> {
        for patch in &output.component_patches {
            let component = self.components.get(&patch.component_type).ok_or_else(|| {
                GameModuleRunError::MissingComponent(patch.component_type.clone())
            })?;
            let value_json = serde_json::to_vec(&patch.value).map_err(|source| {
                GameModuleRunError::InvocationSerialize {
                    system_id: system_id.to_owned(),
                    source,
                }
            })?;
            let mut error = [0_u8; ERROR_BUFFER_SIZE];
            // SAFETY: Module loading validated this callback pointer. The
            // serialized value and error buffer satisfy its byte contracts.
            let valid = unsafe {
                (component.validate)(
                    value_json.as_ptr(),
                    value_json.len(),
                    error.as_mut_ptr(),
                    error.len(),
                )
            };
            if !valid {
                return Err(GameModuleRunError::ComponentPatchValidation {
                    system_id: system_id.to_owned(),
                    component_type: patch.component_type.clone(),
                    message: ffi_error_message(&error),
                });
            }
        }
        Ok(())
    }

    fn validate_resource_patch_values(
        &self,
        system_id: &str,
        output: &GameInvocationOutput,
    ) -> Result<(), GameModuleRunError> {
        for patch in &output.resource_patches {
            let resource = self
                .resources
                .get(&patch.resource_id)
                .ok_or_else(|| GameModuleRunError::MissingResource(patch.resource_id.clone()))?;
            validate_resource_value(&resource.schema, &patch.value).map_err(|message| {
                GameModuleRunError::ResourcePatchValidation {
                    system_id: system_id.to_owned(),
                    resource_id: patch.resource_id.clone(),
                    message,
                }
            })?;
        }
        Ok(())
    }

    fn run_callback(
        &self,
        system: &LoadedSystem,
        invocation: &GameInvocation,
    ) -> Result<GameCallbackOutput, GameModuleRunError> {
        let input = serde_json::to_vec(invocation).map_err(|source| {
            GameModuleRunError::InvocationSerialize {
                system_id: system.metadata.id.clone(),
                source,
            }
        })?;
        validate_game_input_bytes(input.len()).map_err(|source| {
            GameModuleRunError::InvocationLimit {
                system_id: system.metadata.id.clone(),
                source,
            }
        })?;
        let mut output = GameBufferAbi {
            pointer: std::ptr::null_mut(),
            length: 0,
            capacity: 0,
        };
        let mut error = [0_u8; ERROR_BUFFER_SIZE];
        // SAFETY: Load validated the ABI. Input is readable and the output
        // descriptor plus error buffer are writable for the callback.
        let callback_started = Instant::now();
        let success = unsafe {
            (system.run)(
                input.as_ptr(),
                input.len(),
                &mut output,
                error.as_mut_ptr(),
                error.len(),
            )
        };
        let callback_time = callback_started.elapsed();
        if !success {
            if !output.pointer.is_null() {
                // SAFETY: A module that allocated an error-path buffer still
                // owns it through the descriptor's matching free callback.
                unsafe { (self.free_buffer)(output) };
            }
            return Err(GameModuleRunError::System {
                name: system.metadata.rust_name.clone(),
                message: ffi_error_message(&error),
            });
        }
        if output.pointer.is_null() {
            return Err(GameModuleRunError::System {
                name: system.metadata.rust_name.clone(),
                message: "system returned a null output buffer".to_owned(),
            });
        }
        if output.length > output.capacity {
            return Err(GameModuleRunError::System {
                name: system.metadata.rust_name.clone(),
                message: format!(
                    "system returned invalid buffer length {} above capacity {}",
                    output.length, output.capacity
                ),
            });
        }
        if let Err(source) = validate_game_output_bytes(output.length) {
            // SAFETY: The successful callback returned a library-owned buffer.
            unsafe { (self.free_buffer)(output) };
            return Err(GameModuleRunError::OutputLimit {
                system_id: system.metadata.id.clone(),
                source,
            });
        }
        // SAFETY: A successful callback returns `length` readable bytes
        // retained until the matching library free callback runs.
        let output_byte_count = output.length;
        let output_bytes = unsafe { std::slice::from_raw_parts(output.pointer, output.length) };
        let decoded =
            serde_json::from_slice::<GameInvocationOutput>(output_bytes).map_err(|source| {
                GameModuleRunError::OutputDeserialize {
                    system_id: system.metadata.id.clone(),
                    source,
                }
            });
        // SAFETY: The descriptor's free function owns this allocation.
        unsafe { (self.free_buffer)(output) };
        decoded.map(|output| GameCallbackOutput {
            output,
            callback_time,
            input_bytes: input.len(),
            output_bytes: output_byte_count,
        })
    }
}

fn ffi_error_message(buffer: &[u8]) -> String {
    let end = buffer
        .iter()
        .position(|byte| *byte == 0)
        .unwrap_or(buffer.len());
    let message = String::from_utf8_lossy(&buffer[..end]).into_owned();
    if message.is_empty() {
        "game module returned an unspecified error".to_owned()
    } else {
        message
    }
}

fn read_ffi_string(pointer: *const c_char, label: &str) -> Result<String, GameModuleLoadError> {
    if pointer.is_null() {
        return Err(GameModuleLoadError::NullField(label.to_owned()));
    }
    // SAFETY: Descriptor strings are immutable, NUL-terminated library storage.
    unsafe { CStr::from_ptr(pointer) }
        .to_str()
        .map(str::to_owned)
        .map_err(|source| GameModuleLoadError::InvalidUtf8 {
            field: label.to_owned(),
            source,
        })
}

fn read_ffi_slice<'a, T>(
    pointer: *const T,
    count: usize,
    label: &str,
) -> Result<&'a [T], GameModuleLoadError> {
    if count == 0 {
        return Ok(&[]);
    }
    if pointer.is_null() {
        return Err(GameModuleLoadError::NullField(label.to_owned()));
    }
    // SAFETY: The root descriptor promises this immutable library-owned array.
    Ok(unsafe { std::slice::from_raw_parts(pointer, count) })
}

/// World resource retaining a loaded module while its component values exist.
#[derive(Debug, Clone)]
pub struct GameModuleResource {
    /// Shared loaded-library handle and registration metadata.
    pub module: Arc<GameModule>,
}

/// Errors reported while loading and validating a native game library.
#[derive(Debug)]
pub enum GameModuleLoadError {
    /// The dynamic library could not be opened.
    #[cfg(not(target_arch = "wasm32"))]
    Open {
        /// Attempted path.
        path: PathBuf,
        /// Platform loader error.
        source: libloading::Error,
    },
    /// The required export was absent.
    #[cfg(not(target_arch = "wasm32"))]
    MissingEntry(libloading::Error),
    /// The export returned null.
    NullDescriptor,
    /// A required descriptor field was null.
    NullField(String),
    /// A descriptor string was not UTF-8.
    InvalidUtf8 {
        /// Field label.
        field: String,
        /// Decode error.
        source: std::str::Utf8Error,
    },
    /// Host and library ABI differ.
    AbiMismatch {
        /// Host version.
        expected: u32,
        /// Library version.
        found: u32,
    },
    /// Host and library SDK differ.
    SdkMismatch {
        /// Host fingerprint.
        expected: String,
        /// Library fingerprint.
        found: String,
    },
    /// A component ID is invalid.
    InvalidComponentId {
        /// Rejected ID.
        value: String,
        /// Validation error.
        source: engine_authoring::id::ComponentTypeIdError,
    },
    /// A component schema is invalid.
    InvalidSchema {
        /// Component ID.
        component_type: ComponentTypeId,
        /// JSON error.
        source: serde_json::Error,
    },
    /// Descriptor and schema IDs differ.
    SchemaIdMismatch {
        /// Descriptor ID.
        descriptor: ComponentTypeId,
        /// Schema ID.
        schema: ComponentTypeId,
    },
    /// A component ID was exported twice.
    DuplicateComponent(ComponentTypeId),
    /// A project resource schema or default is invalid.
    InvalidResourceSchema {
        /// Rejected stable resource ID.
        resource_id: String,
        /// Schema validation failure.
        message: String,
    },
    /// A project resource ID was exported twice.
    DuplicateResource(String),
    /// A system name was exported twice.
    DuplicateSystem(String),
    /// A system stable ID, alias, or constraint target is invalid.
    InvalidSystemId {
        /// Rejected identifier text.
        value: String,
        /// Validation error.
        source: engine_ecs::SystemIdError,
    },
    /// A system metadata JSON document could not be decoded.
    InvalidSystemMetadata {
        /// JSON decode error.
        source: serde_json::Error,
    },
    /// A system access manifest is invalid or ambiguous.
    InvalidSystemAccess {
        /// Stable ID of the rejected system.
        system_id: String,
        /// Access declaration validation error.
        source: GameAccessError,
    },
    /// A system query references a project component absent from the module.
    UnknownAccessComponent {
        /// Stable ID of the rejected system.
        system_id: String,
        /// Missing exported component type.
        component_type: ComponentTypeId,
    },
    /// A system access references a project resource absent from the module.
    UnknownAccessResource {
        /// Stable ID of the rejected system.
        system_id: String,
        /// Missing exported resource ID.
        resource_id: String,
    },
    /// A system schedule value is unknown.
    InvalidSchedule(u32),
}

impl fmt::Display for GameModuleLoadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            #[cfg(not(target_arch = "wasm32"))]
            Self::Open { path, source } => {
                write!(f, "could not open game module {}: {source}", path.display())
            }
            #[cfg(not(target_arch = "wasm32"))]
            Self::MissingEntry(source) => write!(f, "game module entry is missing: {source}"),
            Self::NullDescriptor => f.write_str("game module returned a null descriptor"),
            Self::NullField(field) => write!(f, "game module `{field}` pointer is null"),
            Self::InvalidUtf8 { field, source } => {
                write!(f, "game module `{field}` is not UTF-8: {source}")
            }
            Self::AbiMismatch { expected, found } => write!(
                f,
                "game module ABI mismatch: host requires {expected}, library exports {found}"
            ),
            Self::SdkMismatch { expected, found } => write!(
                f,
                "game module SDK mismatch: host `{expected}`, library `{found}`"
            ),
            Self::InvalidComponentId { value, source } => {
                write!(f, "invalid game component ID `{value}`: {source}")
            }
            Self::InvalidSchema {
                component_type,
                source,
            } => write!(f, "invalid schema for `{component_type}`: {source}"),
            Self::SchemaIdMismatch { descriptor, schema } => write!(
                f,
                "component descriptor ID `{descriptor}` does not match schema ID `{schema}`"
            ),
            Self::DuplicateComponent(id) => write!(f, "duplicate game component ID `{id}`"),
            Self::InvalidResourceSchema {
                resource_id,
                message,
            } => write!(f, "invalid game resource schema `{resource_id}`: {message}"),
            Self::DuplicateResource(id) => write!(f, "duplicate game resource ID `{id}`"),
            Self::DuplicateSystem(name) => write!(f, "duplicate game system `{name}`"),
            Self::InvalidSystemId { value, source } => {
                write!(f, "invalid game system ID `{value}`: {source}")
            }
            Self::InvalidSystemMetadata { source } => {
                write!(f, "invalid game system metadata: {source}")
            }
            Self::InvalidSystemAccess { system_id, source } => {
                write!(f, "invalid access declaration for `{system_id}`: {source}")
            }
            Self::UnknownAccessComponent {
                system_id,
                component_type,
            } => write!(
                f,
                "game system `{system_id}` queries unregistered component `{component_type}`"
            ),
            Self::UnknownAccessResource {
                system_id,
                resource_id,
            } => write!(
                f,
                "game system `{system_id}` accesses unregistered resource `{resource_id}`"
            ),
            Self::InvalidSchedule(value) => {
                write!(f, "game system has unknown schedule value {value}")
            }
        }
    }
}
impl std::error::Error for GameModuleLoadError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::InvalidSystemId { source, .. } => Some(source),
            Self::InvalidSystemMetadata { source } => Some(source),
            Self::InvalidSystemAccess { source, .. } => Some(source),
            _ => None,
        }
    }
}

/// Errors returned by project component or system callbacks.
#[derive(Debug)]
pub enum GameModuleRunError {
    /// No definition exists for a persisted component.
    MissingComponent(ComponentTypeId),
    /// No schema exists for a requested project resource.
    MissingResource(String),
    /// The requested loaded-system index does not exist.
    UnknownSystemIndex(usize),
    /// Component JSON serialization failed.
    SerializeComponent {
        /// Component ID.
        component_type: ComponentTypeId,
        /// JSON error.
        source: serde_json::Error,
    },
    /// Component spawn failed.
    ComponentSpawn {
        /// Component ID.
        component_type: ComponentTypeId,
        /// Callback reason.
        message: String,
    },
    /// A callback returned a project-component value that failed its Rust schema.
    ComponentPatchValidation {
        /// Stable callback ID.
        system_id: String,
        /// Component whose replacement value was invalid.
        component_type: ComponentTypeId,
        /// Typed module validation message.
        message: String,
    },
    /// A callback returned a resource value that failed its exported schema.
    ResourcePatchValidation {
        /// Stable callback ID.
        system_id: String,
        /// Resource whose replacement value was invalid.
        resource_id: String,
        /// Host schema validation message.
        message: String,
    },
    /// Engine-owned component-store insertion failed.
    ComponentStore {
        /// ECS mutation error.
        source: engine_ecs::WorldError,
    },
    /// Host compilation of declared callback input failed.
    InvocationCompile {
        /// Stable callback ID.
        system_id: String,
        /// Scoped host compiler failure.
        source: GameHostCompileError,
    },
    /// Invocation JSON serialization failed.
    InvocationSerialize {
        /// Stable callback ID.
        system_id: String,
        /// JSON serialization failure.
        source: serde_json::Error,
    },
    /// Encoded invocation input exceeded an ABI cap.
    InvocationLimit {
        /// Stable callback ID.
        system_id: String,
        /// Bounded payload failure.
        source: GameIoLimitError,
    },
    /// Encoded callback output exceeded an ABI cap.
    OutputLimit {
        /// Stable callback ID.
        system_id: String,
        /// Bounded payload failure.
        source: GameIoLimitError,
    },
    /// Callback output JSON was invalid.
    OutputDeserialize {
        /// Stable callback ID.
        system_id: String,
        /// JSON decode failure.
        source: serde_json::Error,
    },
    /// Host validation or atomic application of output failed.
    OutputApply {
        /// Stable callback ID.
        system_id: String,
        /// Scoped output validation failure.
        source: GameHostApplyError,
    },
    /// Deferred command payload or target preflight failed.
    CommandPreparation {
        /// Stable callback ID.
        system_id: String,
        /// Host command validation failure.
        source: GameCommandError,
    },
    /// System execution failed.
    System {
        /// System name.
        name: String,
        /// Callback reason.
        message: String,
    },
}

impl fmt::Display for GameModuleRunError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingComponent(id) => write!(f, "game component `{id}` is not registered"),
            Self::MissingResource(id) => write!(f, "game resource `{id}` is not registered"),
            Self::UnknownSystemIndex(index) => {
                write!(f, "game system index {index} is not registered")
            }
            Self::SerializeComponent {
                component_type,
                source,
            } => write!(f, "could not serialize `{component_type}`: {source}"),
            Self::ComponentSpawn {
                component_type,
                message,
            } => write!(f, "could not spawn `{component_type}`: {message}"),
            Self::ComponentPatchValidation {
                system_id,
                component_type,
                message,
            } => write!(
                f,
                "game system `{system_id}` returned invalid `{component_type}` data: {message}"
            ),
            Self::ResourcePatchValidation {
                system_id,
                resource_id,
                message,
            } => write!(
                f,
                "game system `{system_id}` returned invalid resource `{resource_id}` data: {message}"
            ),
            Self::ComponentStore { source } => {
                write!(f, "could not store game component data: {source}")
            }
            Self::InvocationCompile { system_id, source } => {
                write!(f, "could not compile input for `{system_id}`: {source}")
            }
            Self::InvocationSerialize { system_id, source } => {
                write!(f, "could not serialize input for `{system_id}`: {source}")
            }
            Self::InvocationLimit { system_id, source } => {
                write!(f, "input for `{system_id}` exceeds an ABI limit: {source}")
            }
            Self::OutputLimit { system_id, source } => {
                write!(
                    f,
                    "output from `{system_id}` exceeds an ABI limit: {source}"
                )
            }
            Self::OutputDeserialize { system_id, source } => {
                write!(f, "output from `{system_id}` is invalid JSON: {source}")
            }
            Self::OutputApply { system_id, source } => {
                write!(f, "output from `{system_id}` was rejected: {source}")
            }
            Self::CommandPreparation { system_id, source } => {
                write!(f, "commands from `{system_id}` were rejected: {source}")
            }
            Self::System { name, message } => write!(f, "game system `{name}` failed: {message}"),
        }
    }
}
impl std::error::Error for GameModuleRunError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::SerializeComponent { source, .. } => Some(source),
            Self::ComponentStore { source } => Some(source),
            Self::InvocationCompile { source, .. } => Some(source),
            Self::InvocationSerialize { source, .. } => Some(source),
            Self::InvocationLimit { source, .. } | Self::OutputLimit { source, .. } => Some(source),
            Self::OutputDeserialize { source, .. } => Some(source),
            Self::OutputApply { source, .. } => Some(source),
            Self::CommandPreparation { source, .. } => Some(source),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mission_resource_schema() -> GameResourceSchema {
        GameResourceSchema {
            id: "game.mission".to_owned(),
            display_name: "Mission State".to_owned(),
            description: "Runtime-only mission progress.".to_owned(),
            version: 1,
            fields: vec![
                FieldSchema {
                    name: "phase".to_owned(),
                    display_name: "Phase".to_owned(),
                    description: "Current phase.".to_owned(),
                    field_type: FieldType::I64,
                    required: true,
                    default_value: Some(Value::I64(0)),
                },
                FieldSchema {
                    name: "active".to_owned(),
                    display_name: "Active".to_owned(),
                    description: "Whether the mission is active.".to_owned(),
                    field_type: FieldType::Bool,
                    required: true,
                    default_value: Some(Value::Bool(false)),
                },
            ],
            default_value: Value::Object(BTreeMap::from([
                ("active".to_owned(), Value::Bool(false)),
                ("phase".to_owned(), Value::I64(0)),
            ])),
        }
    }

    fn copy_frame_index(input: &GameInvocation, output: &mut GameInvocationOutput) {
        output
            .resource_patches
            .push(crate::game_io::GameResourcePatch {
                resource_id: "game.frame".to_owned(),
                value: Value::U64(input.clock.frame_index),
            });
    }

    #[test]
    fn vector_fields_roundtrip_through_authoring_values() {
        let value = glam::Vec3::new(1.0, 2.5, -3.0);
        assert_eq!(glam::Vec3::from_value(&value.to_value()).unwrap(), value);
    }
    #[test]
    fn resource_schema_accepts_typed_defaults_and_rejects_invalid_patches() {
        let schema = mission_resource_schema();
        validate_resource_schema(&schema).unwrap();

        let invalid = Value::Object(BTreeMap::from([
            ("active".to_owned(), Value::Bool(true)),
            ("phase".to_owned(), Value::String("two".to_owned())),
        ]));
        assert_eq!(
            validate_resource_value(&schema, &invalid).unwrap_err(),
            "field `phase`: expected a signed integer"
        );
    }
    #[test]
    fn system_metadata_missing_a_current_field_is_rejected() {
        // The exporter and this loader ship in the same SDK build, so metadata
        // that omits a field comes from a stale library rather than from an
        // accepted shorter shape.
        let json = r#"{
            "id":"game.empty",
            "rust_name":"empty",
            "display_name":"Empty",
            "schedule":"update"
        }"#;
        let error = serde_json::from_str::<GameSystemMetadataEnvelope>(json)
            .expect_err("incomplete system metadata must be rejected");
        assert!(error.to_string().contains("missing field"));
    }
    #[test]
    fn exported_descriptor_is_stable_and_versioned() {
        let first = exported_descriptor();
        assert_eq!(first, exported_descriptor());
        // SAFETY: The export points to immutable process-lifetime storage.
        assert_eq!(unsafe { (*first).abi_version }, GAME_MODULE_ABI_VERSION);
    }

    #[test]
    fn scoped_ffi_callback_returns_only_declared_output_envelope() {
        let invocation = GameInvocation {
            schema_version: crate::game_io::GAME_IO_SCHEMA_VERSION,
            system_id: "game.copy_frame".to_owned(),
            clock: crate::game_io::GameClock {
                frame_index: 42,
                ..crate::game_io::GameClock::default()
            },
            input_actions: BTreeMap::new(),
            save_values: BTreeMap::new(),
            queries: Vec::new(),
            resources: BTreeMap::new(),
            host_views: BTreeMap::new(),
            events: Vec::new(),
        };
        let input = serde_json::to_vec(&invocation).unwrap();
        let mut output = GameBufferAbi {
            pointer: std::ptr::null_mut(),
            length: 0,
            capacity: 0,
        };
        let mut error = [0_u8; ERROR_BUFFER_SIZE];

        // SAFETY: Test buffers satisfy the same readable and writable ranges
        // promised by the host-side ABI dispatcher.
        let succeeded = unsafe {
            run_system_ffi(
                input.as_ptr(),
                input.len(),
                &mut output,
                error.as_mut_ptr(),
                error.len(),
                copy_frame_index,
            )
        };

        assert!(succeeded, "{}", ffi_error_message(&error));
        // SAFETY: A successful callback owns `length` initialized bytes until
        // the matching module free function is called below.
        let bytes = unsafe { std::slice::from_raw_parts(output.pointer, output.length) };
        let decoded: GameInvocationOutput = serde_json::from_slice(bytes).unwrap();
        assert_eq!(decoded.resource_patches.len(), 1);
        assert_eq!(decoded.resource_patches[0].value, Value::I64(42));
        // SAFETY: This buffer was allocated by run_system_ffi exactly once.
        unsafe { free_game_buffer_ffi(output) };
    }
}
