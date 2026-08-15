//! Host-independent native project-module registration and C ABI export contracts.
//!
//! The high-level `engine` crate retains dynamic-library loading plus ECS
//! `World` compilation and output application.

use crate::game_contracts::{GameComponent, GameResourceSchema, GameSystemSchedule};
use crate::game_io::{
    validate_game_input_bytes, validate_game_output_bytes, GameInvocation, GameInvocationOutput,
    GameSystemAccess,
};
use engine_authoring::schema::ComponentSchema;
use engine_authoring::value::Value;
use serde::{Deserialize, Serialize};
use std::ffi::{c_char, CString};
use std::sync::OnceLock;

/// Current native game-module ABI version.
pub const GAME_MODULE_ABI_VERSION: u32 = 3;

/// NUL-terminated SDK fingerprint shared by the exporter and host loader.
pub const GAME_MODULE_SDK_FINGERPRINT: &[u8] =
    concat!("iroha-engine-", env!("CARGO_PKG_VERSION"), "\0").as_bytes();

/// Returns the fixed library file name used inside runnable packages.
pub fn packaged_game_module_file_name() -> &'static str {
    if cfg!(target_os = "windows") {
        "game_module.dll"
    } else if cfg!(target_os = "macos") {
        "libgame_module.dylib"
    } else {
        "libgame_module.so"
    }
}

/// C ABI callback used to validate one component authoring value.
pub type GameComponentValidateAbi = unsafe extern "C" fn(*const u8, usize, *mut u8, usize) -> bool;

/// Plugin-owned byte allocation returned through the C ABI.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
#[doc(hidden)]
#[allow(missing_docs)]
pub struct GameBufferAbi {
    pub pointer: *mut u8,
    pub length: usize,
    pub capacity: usize,
}

/// C ABI callback used to execute one project system.
pub type GameSystemRunAbi =
    unsafe extern "C" fn(*const u8, usize, *mut GameBufferAbi, *mut u8, usize) -> bool;

/// Compile-time component registration collected inside a game library.
pub struct GameComponentRegistration {
    /// Builds the authoring schema.
    pub schema: fn() -> ComponentSchema,
    /// Validates and decodes one authoring value using the concrete Rust type.
    pub validate: GameComponentValidateAbi,
}

/// Compile-time project resource registration collected inside a game library.
pub struct GameResourceRegistration {
    /// Builds the schema and host-owned Play default.
    pub schema: fn() -> GameResourceSchema,
}

/// Compile-time system registration collected inside a game library.
pub struct GameSystemRegistration {
    /// Stable persisted ID, independent from the Rust function path.
    pub id: &'static str,
    /// Rust function name used for diagnostics and deterministic ordering.
    pub name: &'static str,
    /// Human-readable label shown by the Systems panel.
    pub display_name: &'static str,
    /// Optional explanation of the system's responsibility.
    pub description: &'static str,
    /// Existing engine schedule that runs this system.
    pub schedule: GameSystemSchedule,
    /// User-specified ordering key within the schedule.
    pub order: i32,
    /// Stable IDs that must run after this system.
    pub before: &'static [&'static str],
    /// Stable IDs that must run before this system.
    pub after: &'static [&'static str],
    /// Previous IDs accepted when migrating saved settings.
    pub aliases: &'static [&'static str],
    /// Builds the query, resource, event, and command access declaration.
    pub access: fn() -> GameSystemAccess,
    /// Query-scoped callback generated next to the system function.
    pub run: GameSystemRunAbi,
}

inventory::collect!(GameComponentRegistration);
inventory::collect!(GameResourceRegistration);
inventory::collect!(GameSystemRegistration);

/// C-compatible exported component descriptor.
#[repr(C)]
#[doc(hidden)]
#[allow(missing_docs)]
pub struct GameComponentDescriptorAbi {
    pub type_id: *const c_char,
    pub schema_json: *const c_char,
    pub validate: GameComponentValidateAbi,
}

/// C-compatible exported system descriptor.
#[repr(C)]
#[doc(hidden)]
#[allow(missing_docs)]
pub struct GameSystemDescriptorAbi {
    pub metadata_json: *const c_char,
    pub run: GameSystemRunAbi,
}

/// Serializable native-module metadata for one project system.
///
/// Both sides of this format are produced by the same SDK build: the exporter
/// writes every field, and the host rejects a module whose ABI version or SDK
/// fingerprint differs. No field is therefore optional on read.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GameSystemMetadata {
    /// Stable persisted ID.
    pub id: String,
    /// Rust function name used in callback diagnostics.
    pub rust_name: String,
    /// Human-readable editor label.
    pub display_name: String,
    /// Editor-facing description, empty when the project declares none.
    pub description: String,
    /// Runtime schedule selected by the project.
    pub schedule: GameSystemSchedule,
    /// Numeric ordering key used for the default Game-to-Game order.
    pub order: i32,
    /// IDs that must execute after this system.
    pub before: Vec<String>,
    /// IDs that must execute before this system.
    pub after: Vec<String>,
    /// Previous stable IDs that keep their saved schedule position after the
    /// project renames this system.
    pub aliases: Vec<String>,
    /// Query, resource, event, and command access validated by the host.
    pub access: GameSystemAccess,
}

#[doc(hidden)]
#[allow(missing_docs)]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GameSystemMetadataEnvelope {
    #[serde(flatten)]
    pub metadata: GameSystemMetadata,
    /// Resource schemas exported by this module generation.
    ///
    /// The exporter places these on the first deterministic system metadata
    /// record so the C descriptor layout can remain ABI v3 compatible. Every
    /// record carries the field; later records carry an empty list.
    pub module_resources: Vec<GameResourceSchema>,
}

/// Root C-compatible descriptor exported by a project library.
#[repr(C)]
#[doc(hidden)]
#[allow(missing_docs)]
pub struct GameModuleDescriptorAbi {
    pub abi_version: u32,
    pub sdk_fingerprint: *const c_char,
    pub components: *const GameComponentDescriptorAbi,
    pub component_count: usize,
    pub systems: *const GameSystemDescriptorAbi,
    pub system_count: usize,
    pub free_buffer: unsafe extern "C" fn(GameBufferAbi),
}

// SAFETY: Exported pointers reference immutable process-lifetime storage.
unsafe impl Send for GameComponentDescriptorAbi {}
// SAFETY: Exported pointers reference immutable process-lifetime storage.
unsafe impl Sync for GameComponentDescriptorAbi {}
// SAFETY: Exported pointers reference immutable process-lifetime storage.
unsafe impl Send for GameSystemDescriptorAbi {}
// SAFETY: Exported pointers reference immutable process-lifetime storage.
unsafe impl Sync for GameSystemDescriptorAbi {}
// SAFETY: Exported pointers reference immutable process-lifetime storage.
unsafe impl Send for GameModuleDescriptorAbi {}
// SAFETY: Exported pointers reference immutable process-lifetime storage.
unsafe impl Sync for GameModuleDescriptorAbi {}

struct ExportStorage {
    _strings: Vec<CString>,
    _components: Vec<GameComponentDescriptorAbi>,
    _systems: Vec<GameSystemDescriptorAbi>,
    descriptor: GameModuleDescriptorAbi,
}

/// Returns the exported descriptor for the currently linked game library.
pub fn exported_descriptor() -> *const GameModuleDescriptorAbi {
    static STORAGE: OnceLock<ExportStorage> = OnceLock::new();
    &STORAGE.get_or_init(build_export_storage).descriptor
}

fn build_export_storage() -> ExportStorage {
    let mut registered_components: Vec<_> = inventory::iter::<GameComponentRegistration>
        .into_iter()
        .map(|registration| ((registration.schema)(), registration.validate))
        .collect();
    registered_components.sort_by(|left, right| left.0.type_id.cmp(&right.0.type_id));
    let mut registered_resources: Vec<_> = inventory::iter::<GameResourceRegistration>
        .into_iter()
        .map(|registration| (registration.schema)())
        .collect();
    registered_resources.sort_by(|left, right| left.id.cmp(&right.id));
    let mut registered_systems: Vec<_> = inventory::iter::<GameSystemRegistration>
        .into_iter()
        .collect();
    registered_systems
        .sort_by_key(|registration| (registration.schedule, registration.order, registration.name));

    let mut strings = Vec::new();
    let mut component_string_indices = Vec::new();
    for (schema, _) in &registered_components {
        let type_id_index = strings.len();
        strings.push(CString::new(schema.type_id.as_str()).expect("component IDs contain no NUL"));
        let schema_index = strings.len();
        let json = serde_json::to_string(schema).expect("component schemas must serialize");
        strings.push(CString::new(json).expect("serialized schemas contain no NUL"));
        component_string_indices.push((type_id_index, schema_index));
    }
    let mut system_string_indices = Vec::new();
    for (system_index, registration) in registered_systems.iter().enumerate() {
        system_string_indices.push(strings.len());
        let metadata = GameSystemMetadataEnvelope {
            metadata: GameSystemMetadata {
                id: registration.id.to_owned(),
                rust_name: registration.name.to_owned(),
                display_name: registration.display_name.to_owned(),
                description: registration.description.to_owned(),
                schedule: registration.schedule,
                order: registration.order,
                before: registration
                    .before
                    .iter()
                    .map(|id| (*id).to_owned())
                    .collect(),
                after: registration
                    .after
                    .iter()
                    .map(|id| (*id).to_owned())
                    .collect(),
                aliases: registration
                    .aliases
                    .iter()
                    .map(|id| (*id).to_owned())
                    .collect(),
                access: (registration.access)(),
            },
            // Carry the module-level schemas exactly once. The loader parses
            // all metadata before validating accesses, so declaration order
            // does not affect resource discovery.
            module_resources: if system_index == 0 {
                registered_resources.clone()
            } else {
                Vec::new()
            },
        };
        let json = serde_json::to_string(&metadata).expect("system metadata must serialize");
        strings.push(CString::new(json).expect("serialized system metadata contains no NUL"));
    }
    let components: Vec<_> = registered_components
        .iter()
        .zip(component_string_indices)
        .map(
            |((_, validate), (type_id, schema_json))| GameComponentDescriptorAbi {
                type_id: strings[type_id].as_ptr(),
                schema_json: strings[schema_json].as_ptr(),
                validate: *validate,
            },
        )
        .collect();
    let systems: Vec<_> = registered_systems
        .iter()
        .zip(system_string_indices)
        .map(|(registration, metadata_json)| GameSystemDescriptorAbi {
            metadata_json: strings[metadata_json].as_ptr(),
            run: registration.run,
        })
        .collect();
    let descriptor = GameModuleDescriptorAbi {
        abi_version: GAME_MODULE_ABI_VERSION,
        sdk_fingerprint: GAME_MODULE_SDK_FINGERPRINT.as_ptr().cast(),
        components: components.as_ptr(),
        component_count: components.len(),
        systems: systems.as_ptr(),
        system_count: systems.len(),
        free_buffer: free_game_buffer_ffi,
    };
    ExportStorage {
        _strings: strings,
        _components: components,
        _systems: systems,
        descriptor,
    }
}

/// Generated component validation callback.
///
/// # Safety
/// The host must supply readable JSON bytes and a writable error buffer.
pub unsafe extern "C" fn validate_component_ffi<T: GameComponent>(
    value_json: *const u8,
    value_json_len: usize,
    error_buffer: *mut u8,
    error_buffer_len: usize,
) -> bool {
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        if value_json.is_null() {
            return Err("host passed a null game-component JSON pointer".to_owned());
        }
        // SAFETY: The callback contract guarantees this readable byte range.
        let bytes = unsafe { std::slice::from_raw_parts(value_json, value_json_len) };
        let value: Value = serde_json::from_slice(bytes)
            .map_err(|error| format!("component JSON could not be decoded: {error}"))?;
        T::from_authoring_value(&value).map(|_| ())
    }));
    finish_ffi_result(result, error_buffer, error_buffer_len)
}

/// Runs a generated query-scoped game system callback.
///
/// # Safety
/// The host must supply readable invocation JSON and writable output/error
/// descriptors. Returned bytes must be released through the exported free
/// callback.
pub unsafe fn run_system_ffi(
    input_json: *const u8,
    input_json_len: usize,
    output: *mut GameBufferAbi,
    error_buffer: *mut u8,
    error_buffer_len: usize,
    system: fn(&GameInvocation, &mut GameInvocationOutput),
) -> bool {
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        if input_json.is_null() || output.is_null() {
            return Err("host passed a null game-system invocation pointer".to_owned());
        }
        validate_game_input_bytes(input_json_len).map_err(|error| error.to_string())?;
        // SAFETY: The callback contract guarantees this readable byte range.
        let bytes = unsafe { std::slice::from_raw_parts(input_json, input_json_len) };
        let invocation: GameInvocation = serde_json::from_slice(bytes)
            .map_err(|error| format!("game invocation could not be decoded: {error}"))?;
        if invocation.schema_version != crate::game_io::GAME_IO_SCHEMA_VERSION {
            return Err(format!(
                "game invocation schema mismatch: module requires {}, host sent {}",
                crate::game_io::GAME_IO_SCHEMA_VERSION,
                invocation.schema_version
            ));
        }
        invocation
            .validate_collection_limits()
            .map_err(|error| error.to_string())?;

        let mut callback_output = GameInvocationOutput::default();
        system(&invocation, &mut callback_output);
        callback_output
            .validate_collection_limits()
            .map_err(|error| error.to_string())?;
        let mut bytes = serde_json::to_vec(&callback_output)
            .map_err(|error| format!("game invocation output could not be encoded: {error}"))?;
        validate_game_output_bytes(bytes.len()).map_err(|error| error.to_string())?;
        let buffer = GameBufferAbi {
            pointer: bytes.as_mut_ptr(),
            length: bytes.len(),
            capacity: bytes.capacity(),
        };
        std::mem::forget(bytes);
        // SAFETY: The callback contract guarantees a writable output descriptor.
        unsafe { *output = buffer };
        Ok(())
    }));
    finish_ffi_result(result, error_buffer, error_buffer_len)
}

/// Runs a generated typed game-system callback.
///
/// The typed adapter derives its access manifest from the function parameter
/// types and converts every missing or mismatched value into a callback error
/// before any host mutation is applied.
///
/// # Safety
///
/// The host must supply readable invocation JSON and writable output/error
/// descriptors. Returned bytes must be released through the exported free
/// callback.
pub unsafe fn run_typed_system_ffi(
    input_json: *const u8,
    input_json_len: usize,
    output: *mut GameBufferAbi,
    error_buffer: *mut u8,
    error_buffer_len: usize,
    system: fn(
        &GameInvocation,
        crate::game_api::TypedOutput,
    ) -> Result<(), crate::game_api::GameApiError>,
) -> bool {
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        if input_json.is_null() || output.is_null() {
            return Err("host passed a null game-system invocation pointer".to_owned());
        }
        validate_game_input_bytes(input_json_len).map_err(|error| error.to_string())?;
        // SAFETY: The callback contract guarantees this readable byte range.
        let bytes = unsafe { std::slice::from_raw_parts(input_json, input_json_len) };
        let invocation: GameInvocation = serde_json::from_slice(bytes)
            .map_err(|error| format!("game invocation could not be decoded: {error}"))?;
        if invocation.schema_version != crate::game_io::GAME_IO_SCHEMA_VERSION {
            return Err(format!(
                "game invocation schema mismatch: module requires {}, host sent {}",
                crate::game_io::GAME_IO_SCHEMA_VERSION,
                invocation.schema_version
            ));
        }
        invocation
            .validate_collection_limits()
            .map_err(|error| error.to_string())?;

        let callback_output = crate::game_api::TypedOutput::new();
        system(&invocation, callback_output.clone()).map_err(|error| error.to_string())?;
        let callback_output = callback_output
            .into_output()
            .map_err(|error| error.to_string())?;
        callback_output
            .validate_collection_limits()
            .map_err(|error| error.to_string())?;
        let mut bytes = serde_json::to_vec(&callback_output)
            .map_err(|error| format!("game invocation output could not be encoded: {error}"))?;
        validate_game_output_bytes(bytes.len()).map_err(|error| error.to_string())?;
        let buffer = GameBufferAbi {
            pointer: bytes.as_mut_ptr(),
            length: bytes.len(),
            capacity: bytes.capacity(),
        };
        std::mem::forget(bytes);
        // SAFETY: The callback contract guarantees a writable output descriptor.
        unsafe { *output = buffer };
        Ok(())
    }));
    finish_ffi_result(result, error_buffer, error_buffer_len)
}

/// Releases a callback output buffer allocated by this module.
///
/// # Safety
/// The buffer must come from a callback in this same module generation and
/// must be released exactly once.
#[doc(hidden)]
pub unsafe extern "C" fn free_game_buffer_ffi(buffer: GameBufferAbi) {
    if buffer.pointer.is_null() {
        return;
    }
    // SAFETY: The buffer was created from Vec by run_system_ffi and is released
    // exactly once by the host through this library-owned function.
    drop(unsafe { Vec::from_raw_parts(buffer.pointer, buffer.length, buffer.capacity) });
}

fn finish_ffi_result(
    result: Result<Result<(), String>, Box<dyn std::any::Any + Send>>,
    error_buffer: *mut u8,
    error_buffer_len: usize,
) -> bool {
    match result {
        Ok(Ok(())) => true,
        Ok(Err(error)) => {
            write_ffi_error(error_buffer, error_buffer_len, &error);
            false
        }
        Err(_) => {
            write_ffi_error(error_buffer, error_buffer_len, "game callback panicked");
            false
        }
    }
}

fn write_ffi_error(buffer: *mut u8, buffer_len: usize, message: &str) {
    if buffer.is_null() || buffer_len == 0 {
        return;
    }
    let count = message.len().min(buffer_len.saturating_sub(1));
    // SAFETY: The callback contract guarantees this writable byte range.
    unsafe {
        std::ptr::copy_nonoverlapping(message.as_ptr(), buffer, count);
        *buffer.add(count) = 0;
    }
}
