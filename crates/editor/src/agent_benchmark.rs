//! Versioned GameEngine Agent Benchmark records and evidence-derived local model catalog.
//!
//! Benchmark data is machine-local application data. Records intentionally contain no
//! conversation transcript, retrieved source text, project path, credentials, or model prompt.

use crate::agent_host::{
    AgentEventEvidence, AgentEventKind, AgentRun, AgentRunState, AgentWorkClaim, CompletionReport,
    CompletionStatus,
};
use crate::native_agent::{InstalledModelInventory, NativeMetrics};
use crate::native_agent_runtime::{HarnessPolicy, NATIVE_WRITE_HARNESS_VERSION};
use crate::resource_arbitration::{InferenceWorkload, QualityPreference, TelemetryValue};
use serde::{Deserialize, Serialize};
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

pub(crate) const BENCHMARK_SCHEMA_VERSION: u32 = 4;
const MIN_SUPPORTED_BENCHMARK_SCHEMA_VERSION: u32 = 1;
pub(crate) const BENCHMARK_CORPUS_VERSION: &str = "gameengine-agent-v1";
pub(crate) const BENCHMARK_HARNESS_VERSION: &str = "gameengine-agent-benchmark-harness-v2";
pub(crate) const ACP_AGENT_HARNESS_ID: &str = "gameengine-acp-agent-harness";
pub(crate) const ACP_AGENT_HARNESS_VERSION: &str = "gameengine-acp-agent-harness-v1";
pub(crate) const RAW_MODEL_BENCHMARK_TASK_ID: &str = "raw_model_generation_v1";
pub(crate) const RAW_MODEL_COMPLETION_CRITERIA: &[&str] = &["model_response_completed"];
pub(crate) const WORKLOAD_POLICY_VERSION: &str = "adr0135-workload-policy-v1";
const CATALOG_SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum BenchmarkTaskKind {
    ReadQuestion,
    ProjectInspection,
    CodeImplementation,
    TypedAuthoringMutation,
    ValidationRepair,
    RuntimeInteraction,
    VisualEvaluation,
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct BenchmarkTaskDescriptor {
    pub(crate) id: &'static str,
    pub(crate) label: &'static str,
    pub(crate) kind: BenchmarkTaskKind,
    pub(crate) completion_criteria: &'static [&'static str],
}

pub(crate) const BENCHMARK_TASKS: [BenchmarkTaskDescriptor; 7] = [
    BenchmarkTaskDescriptor {
        id: "read_question_v1",
        label: "Read question",
        kind: BenchmarkTaskKind::ReadQuestion,
        completion_criteria: &["answer_returned", "provenance_reported"],
    },
    BenchmarkTaskDescriptor {
        id: "project_inspection_v1",
        label: "Project inspection",
        kind: BenchmarkTaskKind::ProjectInspection,
        completion_criteria: &["acceptance_criteria", "authoring_validation"],
    },
    BenchmarkTaskDescriptor {
        id: "code_implementation_v1",
        label: "Code implementation",
        kind: BenchmarkTaskKind::CodeImplementation,
        completion_criteria: &["acceptance_criteria", "source_validation"],
    },
    BenchmarkTaskDescriptor {
        id: "typed_authoring_mutation_v1",
        label: "Typed authoring mutation",
        kind: BenchmarkTaskKind::TypedAuthoringMutation,
        completion_criteria: &["acceptance_criteria", "authoring_validation"],
    },
    BenchmarkTaskDescriptor {
        id: "validation_repair_v1",
        label: "Validation and repair",
        kind: BenchmarkTaskKind::ValidationRepair,
        completion_criteria: &["acceptance_criteria", "source_validation"],
    },
    BenchmarkTaskDescriptor {
        id: "runtime_interaction_v1",
        label: "Runtime interaction",
        kind: BenchmarkTaskKind::RuntimeInteraction,
        completion_criteria: &["play_launch", "interaction_scenarios"],
    },
    BenchmarkTaskDescriptor {
        id: "visual_evaluation_v1",
        label: "Visual evaluation",
        kind: BenchmarkTaskKind::VisualEvaluation,
        completion_criteria: &["frame_capture", "visual_evaluation"],
    },
];

pub(crate) fn benchmark_task(id: &str) -> Option<&'static BenchmarkTaskDescriptor> {
    BENCHMARK_TASKS.iter().find(|task| task.id == id)
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct BenchmarkModelIdentity {
    pub(crate) backend_id: String,
    pub(crate) model_id: String,
    pub(crate) model_version: TelemetryValue<String>,
    pub(crate) quantization: TelemetryValue<String>,
    pub(crate) representation_size_bytes: TelemetryValue<u64>,
    pub(crate) backend_runtime_version: TelemetryValue<String>,
}

/// Explicit execution lane introduced by benchmark schema v4.
///
/// Records written before v4 deliberately deserialize with no lane instead of
/// being reinterpreted as one of these migration-era benchmark classes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum BenchmarkLane {
    RawModel,
    AgentHarness,
    CodingAgent,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct BenchmarkAgentRuntimeIdentity {
    pub(crate) runtime_id: String,
    pub(crate) runtime_version: TelemetryValue<String>,
}

impl BenchmarkAgentRuntimeIdentity {
    fn from_acp(identity: &crate::acp_agent_runtime::AcpRuntimeIdentity) -> Self {
        let runtime_version = identity
            .agent_version
            .as_ref()
            .filter(|version| !version.trim().is_empty())
            .cloned()
            .map(TelemetryValue::Measured)
            .unwrap_or(TelemetryValue::Unavailable);
        Self {
            runtime_id: identity.agent_name.clone(),
            runtime_version,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct BenchmarkHarnessIdentity {
    pub(crate) harness_id: String,
    pub(crate) harness_version: String,
    pub(crate) adapter_version: TelemetryValue<String>,
    pub(crate) acp_protocol_version: TelemetryValue<u16>,
    pub(crate) mcp_tool_contract: TelemetryValue<String>,
    pub(crate) permission_profile: TelemetryValue<String>,
}

#[allow(dead_code)]
impl BenchmarkHarnessIdentity {
    pub(crate) fn new(harness_id: impl Into<String>, harness_version: impl Into<String>) -> Self {
        Self {
            harness_id: harness_id.into(),
            harness_version: harness_version.into(),
            adapter_version: TelemetryValue::Unavailable,
            acp_protocol_version: TelemetryValue::Unavailable,
            mcp_tool_contract: TelemetryValue::Unavailable,
            permission_profile: TelemetryValue::Unavailable,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct BenchmarkRuntimeIdentity {
    pub(crate) lane: BenchmarkLane,
    pub(crate) harness: BenchmarkHarnessIdentity,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) agent_runtime: Option<BenchmarkAgentRuntimeIdentity>,
}

#[allow(dead_code)]
impl BenchmarkRuntimeIdentity {
    pub(crate) fn gameengine_acp_agent_harness(
        identity: &crate::acp_agent_runtime::AcpRuntimeIdentity,
    ) -> Self {
        let mut harness =
            BenchmarkHarnessIdentity::new(ACP_AGENT_HARNESS_ID, ACP_AGENT_HARNESS_VERSION);
        harness.mcp_tool_contract = TelemetryValue::Measured(
            crate::acp_agent_runtime::ACP_GAMEENGINE_MCP_TOOL_CONTRACT.to_owned(),
        );
        harness.permission_profile = TelemetryValue::Measured(
            crate::acp_agent_runtime::ACP_RUN_BOUND_PERMISSION_PROFILE.to_owned(),
        );
        Self::acp_agent_harness(harness, identity)
    }

    pub(crate) fn raw_model(harness: BenchmarkHarnessIdentity) -> Self {
        Self {
            lane: BenchmarkLane::RawModel,
            harness,
            agent_runtime: None,
        }
    }

    pub(crate) fn agent_harness(
        harness: BenchmarkHarnessIdentity,
        agent_runtime: BenchmarkAgentRuntimeIdentity,
    ) -> Self {
        Self {
            lane: BenchmarkLane::AgentHarness,
            harness,
            agent_runtime: Some(agent_runtime),
        }
    }

    pub(crate) fn coding_agent(
        harness: BenchmarkHarnessIdentity,
        agent_runtime: BenchmarkAgentRuntimeIdentity,
    ) -> Self {
        Self {
            lane: BenchmarkLane::CodingAgent,
            harness,
            agent_runtime: Some(agent_runtime),
        }
    }

    pub(crate) fn acp_agent_harness(
        mut harness: BenchmarkHarnessIdentity,
        identity: &crate::acp_agent_runtime::AcpRuntimeIdentity,
    ) -> Self {
        harness.acp_protocol_version = TelemetryValue::Measured(identity.protocol_version);
        Self::agent_harness(harness, BenchmarkAgentRuntimeIdentity::from_acp(identity))
    }

    pub(crate) fn acp_coding_agent(
        mut harness: BenchmarkHarnessIdentity,
        identity: &crate::acp_agent_runtime::AcpRuntimeIdentity,
    ) -> Self {
        harness.acp_protocol_version = TelemetryValue::Measured(identity.protocol_version);
        Self::coding_agent(harness, BenchmarkAgentRuntimeIdentity::from_acp(identity))
    }

    pub(crate) fn matches_acp_runtime(
        &self,
        identity: &crate::acp_agent_runtime::AcpRuntimeIdentity,
    ) -> bool {
        if self.lane == BenchmarkLane::RawModel
            || self.harness.acp_protocol_version
                != TelemetryValue::Measured(identity.protocol_version)
        {
            return false;
        }
        let Some(agent_runtime) = self.agent_runtime.as_ref() else {
            return false;
        };
        if agent_runtime.runtime_id != identity.agent_name {
            return false;
        }
        match (
            &agent_runtime.runtime_version,
            identity.agent_version.as_deref(),
        ) {
            (TelemetryValue::Measured(expected), Some(actual)) => expected == actual,
            (TelemetryValue::Unavailable, None) => true,
            _ => false,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct BenchmarkHardwareIdentity {
    pub(crate) platform: String,
    pub(crate) gpu: TelemetryValue<String>,
    pub(crate) total_gpu_memory_bytes: TelemetryValue<u64>,
    pub(crate) total_system_memory_bytes: TelemetryValue<u64>,
}

impl Default for BenchmarkHardwareIdentity {
    fn default() -> Self {
        Self {
            platform: format!("{}-{}", std::env::consts::OS, std::env::consts::ARCH),
            gpu: TelemetryValue::Unavailable,
            total_gpu_memory_bytes: TelemetryValue::Unavailable,
            total_system_memory_bytes: TelemetryValue::Unavailable,
        }
    }
}

impl BenchmarkHardwareIdentity {
    pub(crate) fn from_editor_adapter(name: &str, vendor_id: u32, device_id: u32) -> Self {
        let name = name.trim();
        if name.is_empty() {
            return Self::default();
        }
        let gpu = TelemetryValue::Measured(name.to_owned());

        #[cfg(target_os = "windows")]
        let (total_gpu_memory_bytes, total_system_memory_bytes) =
            windows_hardware_memory(name, vendor_id, device_id);
        #[cfg(not(target_os = "windows"))]
        let (total_gpu_memory_bytes, total_system_memory_bytes) = {
            let _ = (vendor_id, device_id);
            (TelemetryValue::Unavailable, TelemetryValue::Unavailable)
        };

        Self {
            platform: format!("{}-{}", std::env::consts::OS, std::env::consts::ARCH),
            gpu,
            total_gpu_memory_bytes,
            total_system_memory_bytes,
        }
    }
}

#[cfg(target_os = "windows")]
#[derive(Debug, Clone)]
struct WindowsAdapterMemory {
    name: String,
    vendor_id: u32,
    device_id: u32,
    dedicated_video_memory_bytes: u64,
    software: bool,
}

#[cfg(target_os = "windows")]
fn windows_hardware_memory(
    adapter_name: &str,
    vendor_id: u32,
    device_id: u32,
) -> (TelemetryValue<u64>, TelemetryValue<u64>) {
    let gpu_memory = dxgi_adapter_candidates()
        .and_then(|candidates| {
            select_adapter_memory(adapter_name, vendor_id, device_id, &candidates)
        })
        .map(TelemetryValue::Measured)
        .unwrap_or_default();
    let system_memory = total_physical_memory_bytes()
        .filter(|bytes| *bytes > 0)
        .map(TelemetryValue::Measured)
        .unwrap_or_default();
    (gpu_memory, system_memory)
}

/// Largest dedicated device memory this machine reports, when it reports any.
///
/// Managed runtime launch policy needs a device-memory budget without knowing
/// which adapter the Editor renders on, so this deliberately answers the
/// hardware question rather than the presentation one. It reports nothing on
/// platforms where GameEngine has no measurement, and a caller must treat that
/// as unmeasured instead of substituting a default size.
pub(crate) fn largest_device_memory_bytes() -> Option<u64> {
    #[cfg(target_os = "windows")]
    {
        dxgi_adapter_candidates()?
            .iter()
            .filter(|candidate| !candidate.software)
            .map(|candidate| candidate.dedicated_video_memory_bytes)
            .filter(|bytes| *bytes > 0)
            .max()
    }
    #[cfg(not(target_os = "windows"))]
    {
        None
    }
}

#[cfg(target_os = "windows")]
fn select_adapter_memory(
    adapter_name: &str,
    vendor_id: u32,
    device_id: u32,
    candidates: &[WindowsAdapterMemory],
) -> Option<u64> {
    let by_id = candidates
        .iter()
        .filter(|candidate| {
            !candidate.software
                && vendor_id != 0
                && device_id != 0
                && candidate.vendor_id == vendor_id
                && candidate.device_id == device_id
        })
        .collect::<Vec<_>>();
    if let Some(memory) = unanimous_dedicated_memory(&by_id) {
        return Some(memory);
    }

    let normalized = adapter_name.trim().to_ascii_lowercase();
    let by_name = candidates
        .iter()
        .filter(|candidate| {
            !candidate.software && candidate.name.trim().to_ascii_lowercase() == normalized
        })
        .collect::<Vec<_>>();
    unanimous_dedicated_memory(&by_name)
}

/// Accepts a dedicated-memory reading only when every match reports it.
///
/// Windows enumerates one physical adapter more than once on machines with a
/// virtual display driver or multiple outputs, so requiring a single matching
/// candidate rejected a perfectly unambiguous measurement: the duplicates agree
/// on vendor, device, and dedicated memory because they describe the same card.
/// Agreement is what makes a reading trustworthy here, not uniqueness. Two
/// candidates that disagree stay unavailable rather than being reconciled, and
/// a zero reading is still treated as no reading at all.
#[cfg(target_os = "windows")]
fn unanimous_dedicated_memory(candidates: &[&WindowsAdapterMemory]) -> Option<u64> {
    let memory = candidates.first()?.dedicated_video_memory_bytes;
    if memory == 0 {
        return None;
    }
    candidates
        .iter()
        .all(|candidate| candidate.dedicated_video_memory_bytes == memory)
        .then_some(memory)
}

#[cfg(target_os = "windows")]
#[repr(C)]
#[allow(dead_code)]
struct WindowsGuid {
    data1: u32,
    data2: u16,
    data3: u16,
    data4: [u8; 8],
}

#[cfg(target_os = "windows")]
#[repr(C)]
#[allow(dead_code)]
struct WindowsLuid {
    low_part: u32,
    high_part: i32,
}

#[cfg(target_os = "windows")]
#[repr(C)]
#[allow(dead_code)]
struct DxgiAdapterDesc1 {
    description: [u16; 128],
    vendor_id: u32,
    device_id: u32,
    subsys_id: u32,
    revision: u32,
    dedicated_video_memory: usize,
    dedicated_system_memory: usize,
    shared_system_memory: usize,
    adapter_luid: WindowsLuid,
    flags: u32,
}

#[cfg(target_os = "windows")]
#[repr(C)]
#[allow(dead_code)]
struct MemoryStatusEx {
    length: u32,
    memory_load: u32,
    total_physical: u64,
    available_physical: u64,
    total_page_file: u64,
    available_page_file: u64,
    total_virtual: u64,
    available_virtual: u64,
    available_extended_virtual: u64,
}

#[cfg(target_os = "windows")]
type ComRelease = unsafe extern "system" fn(*mut std::ffi::c_void) -> u32;

#[cfg(target_os = "windows")]
type EnumAdapters1 =
    unsafe extern "system" fn(*mut DxgiFactory1, u32, *mut *mut DxgiAdapter1) -> i32;

#[cfg(target_os = "windows")]
type GetDesc1 = unsafe extern "system" fn(*mut DxgiAdapter1, *mut DxgiAdapterDesc1) -> i32;

#[cfg(target_os = "windows")]
#[repr(C)]
#[allow(dead_code)]
struct DxgiFactory1VTable {
    query_interface: *const std::ffi::c_void,
    add_ref: *const std::ffi::c_void,
    release: ComRelease,
    set_private_data: *const std::ffi::c_void,
    set_private_data_interface: *const std::ffi::c_void,
    get_private_data: *const std::ffi::c_void,
    get_parent: *const std::ffi::c_void,
    enum_adapters: *const std::ffi::c_void,
    make_window_association: *const std::ffi::c_void,
    get_window_association: *const std::ffi::c_void,
    create_swap_chain: *const std::ffi::c_void,
    create_software_adapter: *const std::ffi::c_void,
    enum_adapters1: EnumAdapters1,
    is_current: *const std::ffi::c_void,
}

#[cfg(target_os = "windows")]
#[repr(C)]
struct DxgiFactory1 {
    vtable: *const DxgiFactory1VTable,
}

#[cfg(target_os = "windows")]
#[repr(C)]
#[allow(dead_code)]
struct DxgiAdapter1VTable {
    query_interface: *const std::ffi::c_void,
    add_ref: *const std::ffi::c_void,
    release: ComRelease,
    set_private_data: *const std::ffi::c_void,
    set_private_data_interface: *const std::ffi::c_void,
    get_private_data: *const std::ffi::c_void,
    get_parent: *const std::ffi::c_void,
    enum_outputs: *const std::ffi::c_void,
    get_desc: *const std::ffi::c_void,
    check_interface_support: *const std::ffi::c_void,
    get_desc1: GetDesc1,
}

#[cfg(target_os = "windows")]
#[repr(C)]
struct DxgiAdapter1 {
    vtable: *const DxgiAdapter1VTable,
}

#[cfg(target_os = "windows")]
const IID_IDXGI_FACTORY1: WindowsGuid = WindowsGuid {
    data1: 0x770a_ae78,
    data2: 0xf26f,
    data3: 0x4dba,
    data4: [0xa8, 0x29, 0x25, 0x3c, 0x83, 0xd1, 0xb3, 0x87],
};

#[cfg(target_os = "windows")]
const DXGI_ADAPTER_FLAG_SOFTWARE: u32 = 2;

#[cfg(target_os = "windows")]
#[link(name = "dxgi")]
unsafe extern "system" {
    #[link_name = "CreateDXGIFactory1"]
    fn create_dxgi_factory1(
        interface_id: *const WindowsGuid,
        factory: *mut *mut std::ffi::c_void,
    ) -> i32;
}

#[cfg(target_os = "windows")]
#[link(name = "kernel32")]
unsafe extern "system" {
    #[link_name = "GlobalMemoryStatusEx"]
    fn global_memory_status_ex(status: *mut MemoryStatusEx) -> i32;
}

#[cfg(target_os = "windows")]
fn dxgi_adapter_candidates() -> Option<Vec<WindowsAdapterMemory>> {
    let mut factory = std::ptr::null_mut::<std::ffi::c_void>();
    let result = unsafe { create_dxgi_factory1(&IID_IDXGI_FACTORY1, &mut factory) };
    if result < 0 || factory.is_null() {
        return None;
    }
    let factory = factory.cast::<DxgiFactory1>();

    let mut candidates = Vec::new();
    for index in 0..64_u32 {
        let mut adapter = std::ptr::null_mut::<DxgiAdapter1>();
        let result = unsafe { ((*(*factory).vtable).enum_adapters1)(factory, index, &mut adapter) };
        if result < 0 || adapter.is_null() {
            break;
        }

        let mut description = unsafe { std::mem::zeroed::<DxgiAdapterDesc1>() };
        let result = unsafe { ((*(*adapter).vtable).get_desc1)(adapter, &mut description) };
        if result >= 0 {
            let end = description
                .description
                .iter()
                .position(|character| *character == 0)
                .unwrap_or(description.description.len());
            candidates.push(WindowsAdapterMemory {
                name: String::from_utf16_lossy(&description.description[..end]),
                vendor_id: description.vendor_id,
                device_id: description.device_id,
                dedicated_video_memory_bytes: description.dedicated_video_memory as u64,
                software: description.flags & DXGI_ADAPTER_FLAG_SOFTWARE != 0,
            });
        }

        unsafe { ((*(*adapter).vtable).release)(adapter.cast()) };
    }

    unsafe { ((*(*factory).vtable).release)(factory.cast()) };
    Some(candidates)
}

#[cfg(target_os = "windows")]
fn total_physical_memory_bytes() -> Option<u64> {
    let mut status = unsafe { std::mem::zeroed::<MemoryStatusEx>() };
    status.length = std::mem::size_of::<MemoryStatusEx>() as u32;
    let result = unsafe { global_memory_status_ex(&mut status) };
    (result != 0).then_some(status.total_physical)
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct BenchmarkToolBudget {
    pub(crate) max_model_turns: u32,
    pub(crate) max_tool_failures: u32,
    pub(crate) repair_budget: u32,
    pub(crate) permission_budget: Vec<String>,
    #[serde(default)]
    pub(crate) work_claims: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct BenchmarkExecutionIdentity {
    pub(crate) campaign_harness_version: String,
    pub(crate) schedule_policy_version: String,
    pub(crate) comparison_class: String,
    pub(crate) execution_profile: String,
    pub(crate) execution_environment: String,
    pub(crate) fixture_id: String,
    pub(crate) fixture_version: String,
    pub(crate) fixture_instance_id: String,
    pub(crate) sampling_profile: String,
    pub(crate) seed_policy: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) benchmark_runtime: Option<BenchmarkRuntimeIdentity>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct BenchmarkIdentity {
    pub(crate) corpus_version: String,
    pub(crate) task_id: String,
    pub(crate) harness_version: String,
    pub(crate) runtime_harness_version: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) runtime: Option<BenchmarkRuntimeIdentity>,
    pub(crate) model: BenchmarkModelIdentity,
    pub(crate) hardware: BenchmarkHardwareIdentity,
    pub(crate) quality: QualityPreference,
    pub(crate) workload_policy_version: String,
    pub(crate) observed_workload: TelemetryValue<InferenceWorkload>,
    pub(crate) tool_budget: BenchmarkToolBudget,
    pub(crate) completion_criteria: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) execution: Option<BenchmarkExecutionIdentity>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct BenchmarkMetrics {
    pub(crate) acceptance_success: TelemetryValue<bool>,
    pub(crate) completion_success: TelemetryValue<bool>,
    pub(crate) model_turns: TelemetryValue<u64>,
    pub(crate) tool_calls: TelemetryValue<u64>,
    pub(crate) invalid_or_failed_tool_calls: TelemetryValue<u64>,
    pub(crate) code_edits: TelemetryValue<u64>,
    pub(crate) validation_attempts: TelemetryValue<u64>,
    pub(crate) repair_loops: TelemetryValue<u64>,
    pub(crate) play_attempts: TelemetryValue<u64>,
    pub(crate) frame_capture_attempts: TelemetryValue<u64>,
    pub(crate) visual_evaluation_attempts: TelemetryValue<u64>,
    pub(crate) human_interventions: TelemetryValue<u64>,
    pub(crate) elapsed_ms: TelemetryValue<u64>,
    pub(crate) prompt_tokens: TelemetryValue<u64>,
    pub(crate) response_tokens: TelemetryValue<u64>,
    pub(crate) load_latency_ms: TelemetryValue<u64>,
    pub(crate) ttft_ms: TelemetryValue<u64>,
    pub(crate) generation_tokens_per_second_milli: TelemetryValue<u64>,
    pub(crate) peak_backend_gpu_memory_bytes: TelemetryValue<u64>,
    pub(crate) peak_editor_gpu_memory_bytes: TelemetryValue<u64>,
    pub(crate) model_unload_reload_ms: TelemetryValue<u64>,
    pub(crate) renderer_reclaim_resume_ms: TelemetryValue<u64>,
    pub(crate) oom_failures: TelemetryValue<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct BenchmarkRecord {
    pub(crate) schema_version: u32,
    pub(crate) recorded_unix_ms: u64,
    pub(crate) identity: BenchmarkIdentity,
    pub(crate) metrics: BenchmarkMetrics,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ComparisonEquivalence {
    EquivalentModelComparison,
    EquivalentAgentHarnessComparison,
    EquivalentCodingAgentComparison,
    NonEquivalent(Vec<&'static str>),
}

fn model_identity_is_measured(identity: &BenchmarkModelIdentity) -> bool {
    matches!(&identity.model_version, TelemetryValue::Measured(value) if !value.trim().is_empty())
        && matches!(&identity.quantization, TelemetryValue::Measured(value) if !value.trim().is_empty())
        && matches!(identity.representation_size_bytes, TelemetryValue::Measured(value) if value > 0)
        && matches!(&identity.backend_runtime_version, TelemetryValue::Measured(value) if !value.trim().is_empty())
}

fn hardware_identity_is_measured(identity: &BenchmarkHardwareIdentity) -> bool {
    !identity.platform.trim().is_empty()
        && matches!(&identity.gpu, TelemetryValue::Measured(value) if !value.trim().is_empty())
        && matches!(identity.total_gpu_memory_bytes, TelemetryValue::Measured(value) if value > 0)
        && matches!(identity.total_system_memory_bytes, TelemetryValue::Measured(value) if value > 0)
}

fn benchmark_identity_is_measured(identity: &BenchmarkIdentity) -> bool {
    model_identity_is_measured(&identity.model)
        && hardware_identity_is_measured(&identity.hardware)
        && matches!(identity.observed_workload, TelemetryValue::Measured(_))
}

fn measured_identity_text(value: &TelemetryValue<String>) -> Option<&str> {
    match value {
        TelemetryValue::Measured(value) if !value.trim().is_empty() => Some(value.as_str()),
        TelemetryValue::Measured(_)
        | TelemetryValue::ConservativeEstimate(_)
        | TelemetryValue::Unavailable => None,
    }
}

fn execution_contract_matches(
    left: &Option<BenchmarkExecutionIdentity>,
    right: &Option<BenchmarkExecutionIdentity>,
) -> bool {
    match (left, right) {
        (None, None) => true,
        (Some(left), Some(right)) => {
            left.campaign_harness_version == right.campaign_harness_version
                && left.schedule_policy_version == right.schedule_policy_version
                && left.comparison_class == right.comparison_class
                && left.execution_profile == right.execution_profile
                && left.execution_environment == right.execution_environment
                && left.fixture_id == right.fixture_id
                && left.fixture_version == right.fixture_version
                && left.fixture_instance_id == right.fixture_instance_id
                && left.sampling_profile == right.sampling_profile
                && left.seed_policy == right.seed_policy
        }
        _ => false,
    }
}

fn agent_harness_contract_matches(
    left: &BenchmarkRuntimeIdentity,
    right: &BenchmarkRuntimeIdentity,
    differences: &mut Vec<&'static str>,
) {
    let left_harness = &left.harness;
    let right_harness = &right.harness;
    if !matches!(&left_harness.acp_protocol_version, TelemetryValue::Measured(version) if *version > 0)
        || !matches!(&right_harness.acp_protocol_version, TelemetryValue::Measured(version) if *version > 0)
        || left_harness.acp_protocol_version != right_harness.acp_protocol_version
    {
        differences.push("acp_protocol_version");
    }
    if measured_identity_text(&left_harness.mcp_tool_contract).is_none()
        || measured_identity_text(&right_harness.mcp_tool_contract).is_none()
        || left_harness.mcp_tool_contract != right_harness.mcp_tool_contract
    {
        differences.push("mcp_tool_contract");
    }
    if measured_identity_text(&left_harness.permission_profile).is_none()
        || measured_identity_text(&right_harness.permission_profile).is_none()
        || left_harness.permission_profile != right_harness.permission_profile
    {
        differences.push("permission_profile");
    }
}

fn coding_agent_contract_matches(
    left: &BenchmarkRuntimeIdentity,
    right: &BenchmarkRuntimeIdentity,
    differences: &mut Vec<&'static str>,
) {
    if left.harness.acp_protocol_version != right.harness.acp_protocol_version {
        differences.push("acp_protocol_version");
    }
    if measured_identity_text(&left.harness.mcp_tool_contract).is_none()
        || measured_identity_text(&right.harness.mcp_tool_contract).is_none()
        || left.harness.mcp_tool_contract != right.harness.mcp_tool_contract
    {
        differences.push("mcp_tool_contract");
    }
    if measured_identity_text(&left.harness.permission_profile).is_none()
        || measured_identity_text(&right.harness.permission_profile).is_none()
        || left.harness.permission_profile != right.harness.permission_profile
    {
        differences.push("permission_profile");
    }
}

pub(crate) fn comparison_equivalence(
    left: &BenchmarkRecord,
    right: &BenchmarkRecord,
) -> ComparisonEquivalence {
    let mut differences = Vec::new();
    if left.identity.corpus_version != right.identity.corpus_version {
        differences.push("corpus_version");
    }
    if left.identity.task_id != right.identity.task_id {
        differences.push("task_id");
    }
    if left.identity.harness_version != right.identity.harness_version {
        differences.push("harness_version");
    }
    if !hardware_identity_is_measured(&left.identity.hardware)
        || !hardware_identity_is_measured(&right.identity.hardware)
        || left.identity.hardware != right.identity.hardware
    {
        differences.push("hardware");
    }
    if left.identity.quality != right.identity.quality
        || left.identity.workload_policy_version != right.identity.workload_policy_version
        || !matches!(left.identity.observed_workload, TelemetryValue::Measured(_))
        || !matches!(
            right.identity.observed_workload,
            TelemetryValue::Measured(_)
        )
        || left.identity.observed_workload != right.identity.observed_workload
    {
        differences.push("quality_or_workload");
    }
    if left.identity.tool_budget != right.identity.tool_budget {
        differences.push("tool_or_permission_budget");
    }
    if left.identity.completion_criteria != right.identity.completion_criteria {
        differences.push("completion_criteria");
    }
    if !execution_contract_matches(&left.identity.execution, &right.identity.execution) {
        differences.push("execution_identity");
    }

    let equivalence = match (&left.identity.runtime, &right.identity.runtime) {
        (None, None) => {
            if left.identity.runtime_harness_version != right.identity.runtime_harness_version {
                differences.push("harness_version");
            }
            if !model_identity_is_measured(&left.identity.model)
                || !model_identity_is_measured(&right.identity.model)
            {
                differences.push("model_representation");
            }
            if left.identity.model.backend_id != right.identity.model.backend_id
                || left.identity.model.backend_runtime_version
                    != right.identity.model.backend_runtime_version
            {
                differences.push("backend_runtime");
            }
            ComparisonEquivalence::EquivalentModelComparison
        }
        (Some(left_runtime), Some(right_runtime)) if left_runtime.lane == right_runtime.lane => {
            match left_runtime.lane {
                BenchmarkLane::RawModel => {
                    if left.identity.runtime_harness_version
                        != right.identity.runtime_harness_version
                        || left_runtime != right_runtime
                    {
                        differences.push("harness_version");
                    }
                    if !model_identity_is_measured(&left.identity.model)
                        || !model_identity_is_measured(&right.identity.model)
                    {
                        differences.push("model_representation");
                    }
                    if left.identity.model.backend_id != right.identity.model.backend_id
                        || left.identity.model.backend_runtime_version
                            != right.identity.model.backend_runtime_version
                    {
                        differences.push("backend_runtime");
                    }
                    ComparisonEquivalence::EquivalentModelComparison
                }
                BenchmarkLane::AgentHarness => {
                    if !model_identity_is_measured(&left.identity.model)
                        || !model_identity_is_measured(&right.identity.model)
                        || left.identity.model != right.identity.model
                    {
                        differences.push("model_representation");
                    }
                    agent_harness_contract_matches(left_runtime, right_runtime, &mut differences);
                    ComparisonEquivalence::EquivalentAgentHarnessComparison
                }
                BenchmarkLane::CodingAgent => {
                    coding_agent_contract_matches(left_runtime, right_runtime, &mut differences);
                    ComparisonEquivalence::EquivalentCodingAgentComparison
                }
            }
        }
        _ => {
            differences.push("benchmark_lane");
            ComparisonEquivalence::EquivalentModelComparison
        }
    };

    differences.sort_unstable();
    differences.dedup();
    if differences.is_empty() {
        equivalence
    } else {
        ComparisonEquivalence::NonEquivalent(differences)
    }
}

pub(crate) struct BenchmarkStore {
    root: PathBuf,
}

impl BenchmarkStore {
    pub(crate) fn open(root: PathBuf) -> Result<Self, String> {
        fs::create_dir_all(&root).map_err(|error| error.to_string())?;
        Ok(Self { root })
    }

    pub(crate) fn load(&self) -> Result<Vec<BenchmarkRecord>, String> {
        let mut paths = fs::read_dir(&self.root)
            .map_err(|error| error.to_string())?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| error.to_string())?
            .into_iter()
            .map(|entry| entry.path())
            .filter(|path| {
                path.extension()
                    .is_some_and(|extension| extension == std::ffi::OsStr::new("json"))
            })
            .collect::<Vec<_>>();
        paths.sort();
        let mut records = Vec::new();
        for path in paths {
            let bytes = fs::read(&path).map_err(|error| error.to_string())?;
            let record = serde_json::from_slice::<BenchmarkRecord>(&bytes)
                .map_err(|error| format!("{}: {error}", path.display()))?;
            validate_record(&record)?;
            records.push(record);
        }
        Ok(records)
    }

    pub(crate) fn record(&self, record: &BenchmarkRecord) -> Result<PathBuf, String> {
        validate_record(record)?;
        let bytes = serde_json::to_vec_pretty(record).map_err(|error| error.to_string())?;
        let stem = format!(
            "{}-{}-{}",
            record.recorded_unix_ms,
            safe_file_component(&record.identity.task_id),
            safe_file_component(&record.identity.model.model_id),
        );
        for suffix in 0..1_000_u32 {
            let file_name = if suffix == 0 {
                format!("{stem}.json")
            } else {
                format!("{stem}-{suffix}.json")
            };
            let path = self.root.join(file_name);
            match OpenOptions::new().write(true).create_new(true).open(&path) {
                Ok(mut file) => {
                    file.write_all(&bytes).map_err(|error| error.to_string())?;
                    return Ok(path);
                }
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(error) => return Err(error.to_string()),
            }
        }
        Err("could not allocate a unique benchmark record file".to_owned())
    }
}

fn validate_record(record: &BenchmarkRecord) -> Result<(), String> {
    if !(MIN_SUPPORTED_BENCHMARK_SCHEMA_VERSION..=BENCHMARK_SCHEMA_VERSION)
        .contains(&record.schema_version)
    {
        return Err(format!(
            "unsupported benchmark schema version {}",
            record.schema_version
        ));
    }
    if record.schema_version == 1 && record.identity.execution.is_some() {
        return Err("benchmark schema v1 cannot carry campaign execution identity".to_owned());
    }
    if record.schema_version < 4
        && (record.identity.runtime.is_some()
            || record
                .identity
                .execution
                .as_ref()
                .is_some_and(|execution| execution.benchmark_runtime.is_some()))
    {
        return Err("benchmark schemas v1-v3 cannot carry schema-v4 runtime identity".to_owned());
    }
    if let Some(execution) = record.identity.execution.as_ref() {
        if execution.benchmark_runtime != record.identity.runtime {
            return Err(
                "campaign benchmark runtime identity must match the record runtime identity"
                    .to_owned(),
            );
        }
    }
    if let Some(runtime) = record.identity.runtime.as_ref() {
        if runtime.harness.harness_id.trim().is_empty()
            || runtime.harness.harness_version.trim().is_empty()
        {
            return Err("benchmark harness identity must be non-empty".to_owned());
        }
        if record.identity.runtime_harness_version != runtime.harness.harness_version {
            return Err(
                "benchmark runtime harness version must match the explicit harness identity"
                    .to_owned(),
            );
        }
        let exact_or_unavailable = |value: &TelemetryValue<String>| {
            !matches!(value, TelemetryValue::ConservativeEstimate(_))
                && !matches!(value, TelemetryValue::Measured(text) if text.trim().is_empty())
        };
        if !exact_or_unavailable(&runtime.harness.adapter_version)
            || !exact_or_unavailable(&runtime.harness.mcp_tool_contract)
            || !exact_or_unavailable(&runtime.harness.permission_profile)
            || matches!(
                &runtime.harness.acp_protocol_version,
                TelemetryValue::ConservativeEstimate(_) | TelemetryValue::Measured(0)
            )
        {
            return Err(
                "benchmark runtime identity must be exact or explicitly unavailable".to_owned(),
            );
        }
        match runtime.lane {
            BenchmarkLane::RawModel => {
                if runtime.agent_runtime.is_some()
                    || runtime.harness.acp_protocol_version != TelemetryValue::Unavailable
                    || runtime.harness.mcp_tool_contract != TelemetryValue::Unavailable
                    || runtime.harness.permission_profile != TelemetryValue::Unavailable
                {
                    return Err(
                        "raw model benchmark identity cannot carry agent, ACP, MCP, or permission identity"
                            .to_owned(),
                    );
                }
            }
            BenchmarkLane::AgentHarness | BenchmarkLane::CodingAgent => {
                let Some(agent_runtime) = runtime.agent_runtime.as_ref() else {
                    return Err("agent benchmark lane requires agent runtime identity".to_owned());
                };
                if agent_runtime.runtime_id.trim().is_empty()
                    || !exact_or_unavailable(&agent_runtime.runtime_version)
                {
                    return Err(
                        "agent runtime identity must be non-empty and exact or unavailable"
                            .to_owned(),
                    );
                }
            }
        }
    }
    if record.identity.corpus_version != BENCHMARK_CORPUS_VERSION {
        return Err(format!(
            "unsupported benchmark corpus `{}`",
            record.identity.corpus_version
        ));
    }
    let expected = if record.identity.task_id == RAW_MODEL_BENCHMARK_TASK_ID {
        if !matches!(
            record.identity.runtime.as_ref().map(|runtime| runtime.lane),
            Some(BenchmarkLane::RawModel)
        ) {
            return Err(
                "raw model benchmark task requires explicit raw_model runtime identity".to_owned(),
            );
        }
        RAW_MODEL_COMPLETION_CRITERIA
            .iter()
            .map(|criterion| (*criterion).to_owned())
            .collect::<Vec<_>>()
    } else {
        if matches!(
            record.identity.runtime.as_ref().map(|runtime| runtime.lane),
            Some(BenchmarkLane::RawModel)
        ) {
            return Err(
                "raw_model runtime identity cannot relabel an Agent Benchmark corpus task"
                    .to_owned(),
            );
        }
        let Some(task) = benchmark_task(&record.identity.task_id) else {
            return Err(format!(
                "unknown benchmark task `{}`",
                record.identity.task_id
            ));
        };
        task.completion_criteria
            .iter()
            .map(|criterion| (*criterion).to_owned())
            .collect::<Vec<_>>()
    };
    if record.identity.completion_criteria != expected {
        return Err(
            "benchmark completion criteria do not match the versioned benchmark task".to_owned(),
        );
    }
    if record.identity.model.backend_id.trim().is_empty()
        || record.identity.model.model_id.trim().is_empty()
    {
        return Err("benchmark backend and model identity must be non-empty".to_owned());
    }
    Ok(())
}

#[derive(Debug, Clone, Deserialize)]
struct CatalogManifest {
    schema_version: u32,
    catalog_version: String,
    #[serde(default)]
    entries: Vec<CatalogCandidate>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub(crate) struct CatalogCandidate {
    pub(crate) backend_id: String,
    pub(crate) model_id: String,
    pub(crate) model_version: String,
    pub(crate) quantization: String,
    pub(crate) source: String,
    pub(crate) license: String,
    pub(crate) transfer_size_bytes: u64,
    pub(crate) storage_size_bytes: u64,
    pub(crate) memory_guidance: String,
    pub(crate) context_limit: Option<u64>,
    #[serde(default)]
    pub(crate) modalities: Vec<String>,
    #[serde(default)]
    pub(crate) tool_capabilities: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CatalogProfile {
    Lightweight,
    Balanced,
    HighQuality,
}

impl CatalogProfile {
    pub(crate) const ALL: [Self; 3] = [Self::Lightweight, Self::Balanced, Self::HighQuality];

    pub(crate) const fn label(self) -> &'static str {
        match self {
            Self::Lightweight => "Lightweight",
            Self::Balanced => "Balanced",
            Self::HighQuality => "High Quality",
        }
    }
}

#[derive(Debug, Clone)]
pub(crate) struct CatalogRecommendation {
    pub(crate) profile: CatalogProfile,
    pub(crate) candidate: CatalogCandidate,
    pub(crate) benchmark_version: String,
    pub(crate) evidence_runs: usize,
    pub(crate) aggregate_elapsed_ms: u64,
}

#[derive(Debug, Clone)]
pub(crate) struct CuratedModelCatalog {
    pub(crate) catalog_version: String,
    recommendations: Vec<CatalogRecommendation>,
}

impl CuratedModelCatalog {
    pub(crate) fn from_bundled_manifest(records: &[BenchmarkRecord]) -> Result<Self, String> {
        let manifest = serde_json::from_str::<CatalogManifest>(include_str!(
            "../resources/local_model_catalog_v1.json"
        ))
        .map_err(|error| error.to_string())?;
        Self::derive(manifest, records)
    }

    fn derive(manifest: CatalogManifest, records: &[BenchmarkRecord]) -> Result<Self, String> {
        if manifest.schema_version != CATALOG_SCHEMA_VERSION {
            return Err(format!(
                "unsupported local model catalog schema {}",
                manifest.schema_version
            ));
        }
        let mut qualified = Vec::new();
        for candidate in &manifest.entries {
            qualified.extend(qualify_candidate(candidate, records));
        }
        let Some(reference) = qualified.first().cloned() else {
            return Ok(Self {
                catalog_version: manifest.catalog_version,
                recommendations: Vec::new(),
            });
        };
        let comparable = qualified
            .into_iter()
            .filter(|candidate| candidate.context == reference.context)
            .filter(|candidate| candidate.is_equivalent_to(&reference))
            .collect::<Vec<_>>();
        let mut recommendations = Vec::new();
        if let Some(fastest) = comparable
            .iter()
            .min_by_key(|evidence| evidence.aggregate_elapsed_ms)
            .cloned()
        {
            recommendations.push(fastest.recommendation(CatalogProfile::Lightweight));
        }
        if let Some(balanced) = comparable
            .iter()
            .min_by_key(|evidence| {
                evidence
                    .aggregate_elapsed_ms
                    .saturating_add(evidence.repair_penalty_ms)
            })
            .cloned()
        {
            recommendations.push(balanced.recommendation(CatalogProfile::Balanced));
        }
        if let Some(high_quality) = comparable
            .iter()
            .min_by_key(|evidence| (evidence.repair_penalty_ms, evidence.aggregate_elapsed_ms))
            .cloned()
        {
            recommendations.push(high_quality.recommendation(CatalogProfile::HighQuality));
        }
        Ok(Self {
            catalog_version: manifest.catalog_version,
            recommendations,
        })
    }

    pub(crate) fn recommendation(&self, profile: CatalogProfile) -> Option<&CatalogRecommendation> {
        self.recommendations
            .iter()
            .find(|recommendation| recommendation.profile == profile)
    }

    pub(crate) fn profiles_for_model(
        &self,
        backend_id: &str,
        model_id: &str,
    ) -> Vec<CatalogProfile> {
        self.recommendations
            .iter()
            .filter(|recommendation| {
                recommendation.candidate.backend_id == backend_id
                    && recommendation.candidate.model_id == model_id
            })
            .map(|recommendation| recommendation.profile)
            .collect()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct BenchmarkSuiteContext {
    corpus_version: String,
    harness_version: String,
    backend_id: String,
    backend_runtime_version: TelemetryValue<String>,
    hardware: BenchmarkHardwareIdentity,
    quality: QualityPreference,
    workload_policy_version: String,
}

impl BenchmarkSuiteContext {
    fn from_record(record: &BenchmarkRecord) -> Self {
        Self {
            corpus_version: record.identity.corpus_version.clone(),
            harness_version: record.identity.harness_version.clone(),
            backend_id: record.identity.model.backend_id.clone(),
            backend_runtime_version: record.identity.model.backend_runtime_version.clone(),
            hardware: record.identity.hardware.clone(),
            quality: record.identity.quality,
            workload_policy_version: record.identity.workload_policy_version.clone(),
        }
    }
}

#[derive(Debug, Clone)]
struct QualifiedCandidate {
    candidate: CatalogCandidate,
    context: BenchmarkSuiteContext,
    task_records: Vec<BenchmarkRecord>,
    aggregate_elapsed_ms: u64,
    repair_penalty_ms: u64,
}

impl QualifiedCandidate {
    fn recommendation(self, profile: CatalogProfile) -> CatalogRecommendation {
        CatalogRecommendation {
            profile,
            candidate: self.candidate,
            benchmark_version: BENCHMARK_CORPUS_VERSION.to_owned(),
            evidence_runs: self.task_records.len(),
            aggregate_elapsed_ms: self.aggregate_elapsed_ms,
        }
    }

    fn is_equivalent_to(&self, other: &Self) -> bool {
        BENCHMARK_TASKS.iter().all(|task| {
            let left = self
                .task_records
                .iter()
                .find(|record| record.identity.task_id == task.id);
            let right = other
                .task_records
                .iter()
                .find(|record| record.identity.task_id == task.id);
            matches!(
                (left, right),
                (Some(left), Some(right))
                    if comparison_equivalence(left, right)
                        == ComparisonEquivalence::EquivalentModelComparison
            )
        })
    }
}

fn qualify_candidate(
    candidate: &CatalogCandidate,
    records: &[BenchmarkRecord],
) -> Vec<QualifiedCandidate> {
    if candidate.source.trim().is_empty() || candidate.license.trim().is_empty() {
        return Vec::new();
    }
    let matching = records
        .iter()
        .filter(|record| {
            benchmark_identity_is_measured(&record.identity)
                && candidate_matches_record(candidate, record)
        })
        .collect::<Vec<_>>();
    let mut qualified = Vec::new();
    for seed in &matching {
        let context = BenchmarkSuiteContext::from_record(seed);
        if qualified
            .iter()
            .any(|candidate: &QualifiedCandidate| candidate.context == context)
        {
            continue;
        }
        let cohort = matching
            .iter()
            .copied()
            .filter(|record| BenchmarkSuiteContext::from_record(record) == context)
            .collect::<Vec<_>>();
        let mut task_records = Vec::new();
        let mut elapsed = 0_u64;
        let mut repair_penalty = 0_u64;
        let mut complete = true;
        for task in BENCHMARK_TASKS {
            let record = cohort.iter().find(|record| {
                record.identity.task_id == task.id
                    && matches!(
                        record.metrics.completion_success,
                        TelemetryValue::Measured(true)
                    )
            });
            let Some(record) = record.copied() else {
                complete = false;
                break;
            };
            let TelemetryValue::Measured(task_elapsed) = record.metrics.elapsed_ms else {
                complete = false;
                break;
            };
            elapsed = elapsed.saturating_add(task_elapsed);
            if let TelemetryValue::Measured(repairs) = record.metrics.repair_loops {
                repair_penalty = repair_penalty.saturating_add(repairs.saturating_mul(30_000));
            }
            if let TelemetryValue::Measured(interventions) = record.metrics.human_interventions {
                repair_penalty =
                    repair_penalty.saturating_add(interventions.saturating_mul(60_000));
            }
            task_records.push((*record).clone());
        }
        if complete {
            qualified.push(QualifiedCandidate {
                candidate: candidate.clone(),
                context,
                task_records,
                aggregate_elapsed_ms: elapsed,
                repair_penalty_ms: repair_penalty,
            });
        }
    }
    qualified
}

fn candidate_matches_record(candidate: &CatalogCandidate, record: &BenchmarkRecord) -> bool {
    record.identity.model.backend_id == candidate.backend_id
        && record.identity.model.model_id == candidate.model_id
        && matches!(
            &record.identity.model.model_version,
            TelemetryValue::Measured(version) if version == &candidate.model_version
        )
        && matches!(
            &record.identity.model.quantization,
            TelemetryValue::Measured(quantization) if quantization == &candidate.quantization
        )
}

pub(crate) fn model_identity(
    backend_id: &str,
    model_id: &str,
    inventory: Option<&InstalledModelInventory>,
) -> BenchmarkModelIdentity {
    let installed = inventory
        .and_then(|inventory| inventory.models.iter().find(|model| model.name == model_id));
    BenchmarkModelIdentity {
        backend_id: backend_id.to_owned(),
        model_id: model_id.to_owned(),
        model_version: installed
            .and_then(|model| model.digest.clone())
            .map(TelemetryValue::Measured)
            .unwrap_or_default(),
        quantization: installed
            .and_then(|model| model.quantization_level.clone())
            .map(TelemetryValue::Measured)
            .unwrap_or_default(),
        representation_size_bytes: installed
            .and_then(|model| model.size_bytes)
            .map(TelemetryValue::Measured)
            .unwrap_or_default(),
        backend_runtime_version: inventory
            .and_then(|inventory| inventory.backend_version.clone())
            .map(TelemetryValue::Measured)
            .unwrap_or_default(),
    }
}

/// Builds one direct model/runtime benchmark record without AgentRun semantics.
///
/// The caller owns the actual model-only harness and supplies only telemetry it
/// measured. Agent/ACP/MCP identity is structurally absent from this lane.
#[allow(dead_code)]
pub(crate) fn raw_model_record(
    model: BenchmarkModelIdentity,
    hardware: BenchmarkHardwareIdentity,
    quality: QualityPreference,
    workload: InferenceWorkload,
    harness: BenchmarkHarnessIdentity,
    metrics: BenchmarkMetrics,
) -> Result<BenchmarkRecord, String> {
    let runtime_harness_version = harness.harness_version.clone();
    let record = BenchmarkRecord {
        schema_version: BENCHMARK_SCHEMA_VERSION,
        recorded_unix_ms: unix_ms(),
        identity: BenchmarkIdentity {
            corpus_version: BENCHMARK_CORPUS_VERSION.to_owned(),
            task_id: RAW_MODEL_BENCHMARK_TASK_ID.to_owned(),
            harness_version: BENCHMARK_HARNESS_VERSION.to_owned(),
            runtime_harness_version,
            runtime: Some(BenchmarkRuntimeIdentity::raw_model(harness)),
            model,
            hardware,
            quality,
            workload_policy_version: WORKLOAD_POLICY_VERSION.to_owned(),
            observed_workload: TelemetryValue::Measured(workload),
            tool_budget: BenchmarkToolBudget {
                max_model_turns: 0,
                max_tool_failures: 0,
                repair_budget: 0,
                permission_budget: Vec::new(),
                work_claims: Vec::new(),
            },
            completion_criteria: RAW_MODEL_COMPLETION_CRITERIA
                .iter()
                .map(|criterion| (*criterion).to_owned())
                .collect(),
            execution: None,
        },
        metrics,
    };
    validate_record(&record)?;
    Ok(record)
}

pub(crate) fn read_question_record(
    task_id: &str,
    metrics: &NativeMetrics,
    inventory: Option<&InstalledModelInventory>,
    quality: QualityPreference,
    workload: InferenceWorkload,
    hardware: &BenchmarkHardwareIdentity,
) -> Result<BenchmarkRecord, String> {
    let task =
        benchmark_task(task_id).ok_or_else(|| format!("unknown benchmark task `{task_id}`"))?;
    if task.kind != BenchmarkTaskKind::ReadQuestion {
        return Err(
            "the last read-oriented result can only record a read-question benchmark task"
                .to_owned(),
        );
    }
    let provenance_reported = metrics.retrieval_chunks > 0;
    Ok(BenchmarkRecord {
        schema_version: BENCHMARK_SCHEMA_VERSION,
        recorded_unix_ms: unix_ms(),
        identity: BenchmarkIdentity {
            corpus_version: BENCHMARK_CORPUS_VERSION.to_owned(),
            task_id: task.id.to_owned(),
            harness_version: BENCHMARK_HARNESS_VERSION.to_owned(),
            runtime_harness_version: metrics.harness_version.to_owned(),
            runtime: None,
            model: model_identity(metrics.backend_id, &metrics.model_id, inventory),
            hardware: hardware.clone(),
            quality,
            workload_policy_version: WORKLOAD_POLICY_VERSION.to_owned(),
            observed_workload: TelemetryValue::Measured(workload),
            tool_budget: BenchmarkToolBudget {
                max_model_turns: 1,
                max_tool_failures: 0,
                repair_budget: 0,
                permission_budget: vec!["read_only".to_owned()],
                work_claims: Vec::new(),
            },
            completion_criteria: task
                .completion_criteria
                .iter()
                .map(|criterion| (*criterion).to_owned())
                .collect(),
            execution: None,
        },
        metrics: BenchmarkMetrics {
            acceptance_success: TelemetryValue::Measured(provenance_reported),
            completion_success: TelemetryValue::Measured(provenance_reported),
            model_turns: TelemetryValue::Measured(u64::from(metrics.model_turns)),
            tool_calls: TelemetryValue::Measured(0),
            invalid_or_failed_tool_calls: TelemetryValue::Measured(0),
            code_edits: TelemetryValue::Measured(0),
            validation_attempts: TelemetryValue::Measured(0),
            repair_loops: TelemetryValue::Measured(0),
            play_attempts: TelemetryValue::Measured(0),
            frame_capture_attempts: TelemetryValue::Measured(0),
            visual_evaluation_attempts: TelemetryValue::Measured(0),
            human_interventions: TelemetryValue::Measured(0),
            elapsed_ms: TelemetryValue::Measured(metrics.elapsed_ms),
            prompt_tokens: optional_measured(metrics.prompt_eval_tokens),
            response_tokens: optional_measured(metrics.response_tokens),
            load_latency_ms: optional_measured(metrics.load_latency_ms),
            ttft_ms: optional_measured(metrics.ttft_ms),
            generation_tokens_per_second_milli: optional_measured(
                metrics.generation_tokens_per_second_milli,
            ),
            peak_backend_gpu_memory_bytes: TelemetryValue::Unavailable,
            peak_editor_gpu_memory_bytes: TelemetryValue::Unavailable,
            model_unload_reload_ms: TelemetryValue::Unavailable,
            renderer_reclaim_resume_ms: TelemetryValue::Unavailable,
            oom_failures: TelemetryValue::Unavailable,
        },
    })
}

fn work_claim_kind_label(claim: &AgentWorkClaim) -> Option<String> {
    serde_json::to_value(claim.kind)
        .ok()?
        .as_str()
        .map(ToOwned::to_owned)
}

pub(crate) struct AgentRunBenchmarkIdentity<'a> {
    pub(crate) backend_id: &'a str,
    pub(crate) model_id: &'a str,
    pub(crate) inventory: Option<&'a InstalledModelInventory>,
    pub(crate) quality: QualityPreference,
    pub(crate) workload: InferenceWorkload,
    pub(crate) hardware: &'a BenchmarkHardwareIdentity,
}

pub(crate) fn agent_run_record(
    task_id: &str,
    run: &AgentRun,
    identity: AgentRunBenchmarkIdentity<'_>,
) -> Result<BenchmarkRecord, String> {
    let task =
        benchmark_task(task_id).ok_or_else(|| format!("unknown benchmark task `{task_id}`"))?;
    if task.kind == BenchmarkTaskKind::ReadQuestion {
        return Err(
            "write-capable AgentRun evidence cannot be recorded as a read-question task".to_owned(),
        );
    }
    if !matches!(
        run.state,
        AgentRunState::Completed | AgentRunState::Failed | AgentRunState::Cancelled
    ) {
        return Err("benchmark evidence requires a terminal AgentRun".to_owned());
    }
    let policy = HarnessPolicy::default();
    let permission_budget = run
        .proposal_snapshot
        .requested_capabilities
        .iter()
        .filter_map(|capability| serde_json::to_value(capability).ok())
        .filter_map(|value| value.as_str().map(ToOwned::to_owned))
        .collect::<Vec<_>>();
    let tool_calls = run
        .events
        .iter()
        .filter(|event| matches!(&event.evidence, Some(AgentEventEvidence::ToolAction { .. })))
        .count() as u64;
    let failed_tool_calls = run
        .events
        .iter()
        .filter(|event| {
            matches!(
                &event.evidence,
                Some(AgentEventEvidence::ToolAction {
                    success: Some(false),
                    ..
                })
            )
        })
        .count() as u64;
    let play_attempts = run
        .events
        .iter()
        .filter(|event| matches!(&event.evidence, Some(AgentEventEvidence::Playtest { .. })))
        .count() as u64;
    let frame_attempts = run
        .events
        .iter()
        .filter(|event| {
            matches!(
                &event.evidence,
                Some(AgentEventEvidence::CapturedFrame { .. })
            )
        })
        .count() as u64;
    let visual_attempts = run
        .events
        .iter()
        .filter(|event| matches!(&event.evidence, Some(AgentEventEvidence::CompletionGate { gate, .. }) if gate == "visual_evaluation"))
        .count() as u64;
    let human_interventions = run
        .events
        .iter()
        .filter(|event| event.kind == AgentEventKind::UserMessage)
        .count() as u64;
    let elapsed_ms = run
        .finished_unix_ms
        .map(|finished| finished.saturating_sub(run.started_unix_ms));
    let exchanges = model_exchange_metrics(run);
    let completion_success = task_completion_success(task, run);
    Ok(BenchmarkRecord {
        schema_version: BENCHMARK_SCHEMA_VERSION,
        recorded_unix_ms: unix_ms(),
        identity: BenchmarkIdentity {
            corpus_version: BENCHMARK_CORPUS_VERSION.to_owned(),
            task_id: task.id.to_owned(),
            harness_version: BENCHMARK_HARNESS_VERSION.to_owned(),
            runtime_harness_version: NATIVE_WRITE_HARNESS_VERSION.to_owned(),
            runtime: None,
            model: model_identity(identity.backend_id, identity.model_id, identity.inventory),
            hardware: identity.hardware.clone(),
            quality: identity.quality,
            workload_policy_version: WORKLOAD_POLICY_VERSION.to_owned(),
            observed_workload: TelemetryValue::Measured(identity.workload),
            tool_budget: BenchmarkToolBudget {
                max_model_turns: policy.max_model_turns,
                max_tool_failures: policy.max_tool_failures,
                repair_budget: policy.repair_budget,
                permission_budget,
                work_claims: run
                    .proposal_snapshot
                    .work_claims
                    .iter()
                    .filter_map(work_claim_kind_label)
                    .collect(),
            },
            completion_criteria: task
                .completion_criteria
                .iter()
                .map(|criterion| (*criterion).to_owned())
                .collect(),
            execution: None,
        },
        metrics: BenchmarkMetrics {
            acceptance_success: TelemetryValue::Measured(
                run.completion.acceptance_criteria == CompletionStatus::Passed,
            ),
            completion_success: TelemetryValue::Measured(completion_success),
            model_turns: exchanges.turns,
            tool_calls: TelemetryValue::Measured(tool_calls),
            invalid_or_failed_tool_calls: TelemetryValue::Measured(failed_tool_calls),
            code_edits: TelemetryValue::Measured(run.audit.code_changes),
            validation_attempts: TelemetryValue::Measured(run.validation_attempts.len() as u64),
            repair_loops: TelemetryValue::Unavailable,
            play_attempts: TelemetryValue::Measured(play_attempts),
            frame_capture_attempts: TelemetryValue::Measured(frame_attempts),
            visual_evaluation_attempts: TelemetryValue::Measured(visual_attempts),
            human_interventions: TelemetryValue::Measured(human_interventions),
            elapsed_ms: elapsed_ms.map(TelemetryValue::Measured).unwrap_or_default(),
            prompt_tokens: exchanges.prompt_tokens,
            response_tokens: exchanges.response_tokens,
            load_latency_ms: TelemetryValue::Unavailable,
            ttft_ms: TelemetryValue::Unavailable,
            generation_tokens_per_second_milli: TelemetryValue::Unavailable,
            peak_backend_gpu_memory_bytes: TelemetryValue::Unavailable,
            peak_editor_gpu_memory_bytes: TelemetryValue::Unavailable,
            model_unload_reload_ms: TelemetryValue::Unavailable,
            renderer_reclaim_resume_ms: TelemetryValue::Unavailable,
            oom_failures: TelemetryValue::Unavailable,
        },
    })
}

/// Model-internal telemetry supplied by the runtime that actually observed it.
///
/// ACP adapters use [`BenchmarkModelTelemetry::unavailable`] when the agent does
/// not expose model turns or token counts. GameEngine never derives those values
/// from ACP prompt boundaries or normalized agent events.
#[allow(dead_code)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct BenchmarkModelTelemetry {
    pub(crate) model_turns: TelemetryValue<u64>,
    pub(crate) prompt_tokens: TelemetryValue<u64>,
    pub(crate) response_tokens: TelemetryValue<u64>,
}

#[allow(dead_code)]
impl BenchmarkModelTelemetry {
    pub(crate) fn unavailable() -> Self {
        Self {
            model_turns: TelemetryValue::Unavailable,
            prompt_tokens: TelemetryValue::Unavailable,
            response_tokens: TelemetryValue::Unavailable,
        }
    }
}

/// Records a terminal AgentRun under an explicit agent-inclusive benchmark lane.
///
/// `model_telemetry` must come from the runtime that observed the underlying
/// model. Callers that only have ACP-level evidence must pass explicit
/// `Unavailable` values instead of estimating model turns or tokens.
#[allow(dead_code)]
pub(crate) fn agent_run_record_with_runtime(
    task_id: &str,
    run: &AgentRun,
    identity: AgentRunBenchmarkIdentity<'_>,
    runtime: BenchmarkRuntimeIdentity,
    model_telemetry: BenchmarkModelTelemetry,
) -> Result<BenchmarkRecord, String> {
    if runtime.lane == BenchmarkLane::RawModel {
        return Err("AgentRun evidence cannot be registered as a raw model benchmark".to_owned());
    }
    let mut record = agent_run_record(task_id, run, identity)?;
    record.identity.runtime_harness_version = runtime.harness.harness_version.clone();
    record.identity.runtime = Some(runtime);
    record.metrics.model_turns = model_telemetry.model_turns;
    record.metrics.prompt_tokens = model_telemetry.prompt_tokens;
    record.metrics.response_tokens = model_telemetry.response_tokens;
    validate_record(&record)?;
    Ok(record)
}

/// Metrics a legacy native AgentRun can report from its recorded model exchanges (ADR 0159).
///
/// A legacy run with no recorded exchange reports zero turns, which preserves
/// the existing harness meaning. ACP-backed callers must use
/// [`agent_run_record_with_runtime`] and explicit unavailable telemetry instead.
/// Token counts stay unavailable unless every recorded exchange reported them,
/// so a partial sum is never presented as a measurement.
struct ModelExchangeMetrics {
    turns: TelemetryValue<u64>,
    prompt_tokens: TelemetryValue<u64>,
    response_tokens: TelemetryValue<u64>,
}

fn model_exchange_metrics(run: &AgentRun) -> ModelExchangeMetrics {
    let mut turns = 0_u64;
    let mut prompt_total = Some(0_u64);
    let mut response_total = Some(0_u64);
    for event in &run.events {
        let Some(AgentEventEvidence::ModelExchange {
            prompt_tokens,
            response_tokens,
            ..
        }) = event.evidence.as_ref()
        else {
            continue;
        };
        turns = turns.saturating_add(1);
        prompt_total = prompt_total
            .zip(*prompt_tokens)
            .map(|(total, tokens)| total.saturating_add(tokens));
        response_total = response_total
            .zip(*response_tokens)
            .map(|(total, tokens)| total.saturating_add(tokens));
    }
    let measured_or_unavailable = |total: Option<u64>| {
        if turns == 0 {
            return TelemetryValue::Unavailable;
        }
        total
            .map(TelemetryValue::Measured)
            .unwrap_or(TelemetryValue::Unavailable)
    };
    ModelExchangeMetrics {
        turns: TelemetryValue::Measured(turns),
        prompt_tokens: measured_or_unavailable(prompt_total),
        response_tokens: measured_or_unavailable(response_total),
    }
}

fn completion_gate_status(report: &CompletionReport, criterion: &str) -> Option<CompletionStatus> {
    match criterion {
        "acceptance_criteria" => Some(report.acceptance_criteria),
        "authoring_validation" => Some(report.authoring_validation),
        "source_validation" => Some(report.source_validation),
        "play_launch" => Some(report.play_launch),
        "frame_capture" => Some(report.frame_capture),
        "visual_evaluation" => Some(report.visual_evaluation),
        "interaction_scenarios" => Some(report.interaction_scenarios),
        _ => None,
    }
}

fn completion_report_satisfies_task(
    task: &BenchmarkTaskDescriptor,
    report: &CompletionReport,
    validation_attempts: usize,
) -> bool {
    let required_gates_passed = task.completion_criteria.iter().all(|criterion| {
        completion_gate_status(report, criterion) == Some(CompletionStatus::Passed)
    });
    required_gates_passed
        && (task.kind != BenchmarkTaskKind::ValidationRepair || validation_attempts >= 2)
}

fn task_completion_success(task: &BenchmarkTaskDescriptor, run: &AgentRun) -> bool {
    run.state == AgentRunState::Completed
        && completion_report_satisfies_task(task, &run.completion, run.validation_attempts.len())
        && task_required_tool_evidence_satisfied(task, run)
}

fn task_required_tool_evidence_satisfied(task: &BenchmarkTaskDescriptor, run: &AgentRun) -> bool {
    if task.kind != BenchmarkTaskKind::ProjectInspection {
        return true;
    }
    let mut scene_inspected = false;
    let mut source_read = false;
    for evidence in run
        .events
        .iter()
        .filter_map(|event| event.evidence.as_ref())
    {
        let AgentEventEvidence::ToolAction { tool, success, .. } = evidence else {
            continue;
        };
        if *success != Some(true) {
            continue;
        }
        scene_inspected |= matches!(
            tool.as_str(),
            "scene.inspect" | "scene.validate" | "authoring.inspect"
        );
        source_read |= tool == "workspace.code_read";
    }
    scene_inspected && source_read
}

/// Verifies that the project-inspection benchmark has enough host-owned
/// evidence to allow the provider to hand control back to managed validation.
///
/// The provider may emit a syntactically valid `ready_for_validation` action
/// before it has actually inspected anything. Allowing that action to advance
/// the state first hides the actionable cause and makes a model that skipped
/// its tools look like an ordinary completion-gate failure. The native
/// benchmark child uses this predicate at the handoff boundary so missing work
/// is returned to the model as a recoverable policy result instead.
pub(crate) fn benchmark_project_inspection_ready(run: &AgentRun) -> Result<(), String> {
    if run.completion.acceptance_criteria != CompletionStatus::Passed {
        return Err(
            "project_inspection_v1 cannot enter validation before acceptance_criteria is passed"
                .to_owned(),
        );
    }
    if run.completion.authoring_validation != CompletionStatus::Passed {
        return Err(
            "project_inspection_v1 cannot enter validation before authoring_validation is passed"
                .to_owned(),
        );
    }
    let task = benchmark_task("project_inspection_v1")
        .expect("project_inspection_v1 is part of the fixed benchmark corpus");
    if !task_required_tool_evidence_satisfied(task, run) {
        return Err(
            "project_inspection_v1 requires successful scene inspection and workspace.code_read evidence before validation"
                .to_owned(),
        );
    }
    Ok(())
}

fn optional_measured(value: Option<u64>) -> TelemetryValue<u64> {
    value.map(TelemetryValue::Measured).unwrap_or_default()
}

fn unix_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(u128::from(u64::MAX)) as u64
}

fn safe_file_component(value: &str) -> String {
    let mut output = value
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || matches!(character, '-' | '_') {
                character
            } else {
                '-'
            }
        })
        .collect::<String>();
    if output.len() > 80 {
        output.truncate(80);
    }
    if output.is_empty() {
        "unknown".to_owned()
    } else {
        output
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent_host::{AgentEvent, AgentHost, ModelExchangeRecord};

    fn benchmark_temp_path(label: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!(
            "gameengine-benchmark-{label}-{}-{}",
            std::process::id(),
            unix_ms()
        ))
    }

    fn failing_run_with_exchanges(
        label: &str,
        exchanges: u32,
    ) -> (AgentRun, Vec<std::path::PathBuf>) {
        let project = benchmark_temp_path(&format!("{label}-project"));
        let storage = benchmark_temp_path(&format!("{label}-storage"));
        std::fs::create_dir_all(&project).expect("test project directory");
        let mut host = AgentHost::open(project.clone(), storage.clone()).expect("host");
        let session = host.create_session(label).expect("session");
        let proposal_version = host.session(&session).expect("session").proposal.version;
        let run_id = host
            .start_run_authorized(&session, proposal_version, "test")
            .expect("run");
        host.transition_run(&run_id, AgentRunState::Executing, "execute")
            .expect("executing");
        for turn in 1..=exchanges {
            host.record_model_exchange(
                &run_id,
                ModelExchangeRecord {
                    turn,
                    prompt: "prompt",
                    response: "response",
                    prompt_tokens: Some(100),
                    response_tokens: Some(10),
                    finish_reason: "stop",
                    response_digest: "digest",
                    response_excerpt: "response",
                },
            )
            .expect("recorded exchange");
        }
        host.transition_run(&run_id, AgentRunState::Failed, "failed")
            .expect("failed");
        let run = host.run(&run_id).expect("run").clone();
        (run, vec![project, storage])
    }

    fn record_for(run: &AgentRun) -> BenchmarkRecord {
        agent_run_record(
            "project_inspection_v1",
            run,
            AgentRunBenchmarkIdentity {
                backend_id: "test-backend",
                model_id: "test-model",
                inventory: None,
                quality: QualityPreference::Balanced,
                workload: InferenceWorkload::InteractiveReasoning,
                hardware: &BenchmarkHardwareIdentity::default(),
            },
        )
        .expect("record")
    }

    #[test]
    fn a_failing_run_reports_the_model_turns_and_tokens_it_recorded() {
        let (run, directories) = failing_run_with_exchanges("failing-with-output", 3);
        let record = record_for(&run);
        assert_eq!(record.schema_version, BENCHMARK_SCHEMA_VERSION);
        assert_eq!(record.metrics.model_turns, TelemetryValue::Measured(3));
        assert_eq!(record.metrics.prompt_tokens, TelemetryValue::Measured(300));
        assert_eq!(record.metrics.response_tokens, TelemetryValue::Measured(30));
        assert!(matches!(
            record.metrics.model_turns,
            TelemetryValue::Measured(turns) if turns > 0
        ));
        for directory in directories {
            let _ = std::fs::remove_dir_all(directory);
        }
    }

    #[test]
    fn a_run_that_never_answered_is_measured_as_zero_turns_not_as_unmeasured() {
        let (run, directories) = failing_run_with_exchanges("failing-without-output", 0);
        let record = record_for(&run);
        assert_eq!(record.metrics.model_turns, TelemetryValue::Measured(0));
        assert_eq!(record.metrics.prompt_tokens, TelemetryValue::Unavailable);
        assert_eq!(record.metrics.response_tokens, TelemetryValue::Unavailable);
        assert_eq!(record.metrics.model_turns, TelemetryValue::Measured(0));
        for directory in directories {
            let _ = std::fs::remove_dir_all(directory);
        }
    }

    #[test]
    fn project_inspection_requires_scene_and_source_tool_evidence() {
        let (mut run, directories) = failing_run_with_exchanges("inspection-evidence", 1);
        run.state = AgentRunState::Completed;
        run.completion.acceptance_criteria = CompletionStatus::Passed;
        run.completion.authoring_validation = CompletionStatus::Passed;
        let task = benchmark_task("project_inspection_v1").expect("inspection task");

        assert!(!task_completion_success(task, &run));
        assert!(benchmark_project_inspection_ready(&run).is_err());
        run.events.push(AgentEvent {
            sequence: run.events.last().map_or(1, |event| event.sequence + 1),
            created_unix_ms: 0,
            kind: AgentEventKind::ToolAction,
            message: "scene inspected".to_owned(),
            validation: None,
            evidence: Some(AgentEventEvidence::ToolAction {
                tool: "scene.inspect".to_owned(),
                action: "test".to_owned(),
                success: Some(true),
            }),
        });
        assert!(!task_completion_success(task, &run));
        assert!(benchmark_project_inspection_ready(&run).is_err());
        run.events.push(AgentEvent {
            sequence: run.events.last().map_or(1, |event| event.sequence + 1),
            created_unix_ms: 0,
            kind: AgentEventKind::ToolAction,
            message: "source read".to_owned(),
            validation: None,
            evidence: Some(AgentEventEvidence::ToolAction {
                tool: "workspace.code_read".to_owned(),
                action: "test".to_owned(),
                success: Some(true),
            }),
        });
        assert!(task_completion_success(task, &run));
        assert!(benchmark_project_inspection_ready(&run).is_ok());

        for directory in directories {
            let _ = std::fs::remove_dir_all(directory);
        }
    }

    #[test]
    fn a_version_two_record_stays_readable_and_comparable_against_version_three() {
        let mut legacy = BenchmarkRecord {
            schema_version: 2,
            recorded_unix_ms: 1,
            identity: measured_identity("model"),
            metrics: measured_metrics(1_000),
        };
        legacy.metrics.model_turns = TelemetryValue::Unavailable;
        let mut current = legacy.clone();
        current.schema_version = BENCHMARK_SCHEMA_VERSION;
        current.metrics.model_turns = TelemetryValue::Measured(4);
        assert!(validate_record(&legacy).is_ok());
        assert!(validate_record(&current).is_ok());
        assert_eq!(
            comparison_equivalence(&legacy, &current),
            ComparisonEquivalence::EquivalentModelComparison
        );
    }

    fn agent_harness_identity(agent_name: &str) -> BenchmarkRuntimeIdentity {
        let mut harness = BenchmarkHarnessIdentity::new("acp-agent-harness", "harness-v1");
        harness.adapter_version = TelemetryValue::Measured("adapter-v1".to_owned());
        harness.mcp_tool_contract = TelemetryValue::Measured("editor-mcp-contract-v1".to_owned());
        harness.permission_profile =
            TelemetryValue::Measured("benchmark-agent-readwrite-v1".to_owned());
        let acp = crate::acp_agent_runtime::AcpRuntimeIdentity::stable(
            agent_name,
            Some("1.0".to_owned()),
        );
        BenchmarkRuntimeIdentity::acp_agent_harness(harness, &acp)
    }

    fn coding_agent_identity(agent_name: &str) -> BenchmarkRuntimeIdentity {
        let mut harness = BenchmarkHarnessIdentity::new("coding-agent-adapter", "harness-v1");
        harness.adapter_version = TelemetryValue::Measured("adapter-v1".to_owned());
        harness.mcp_tool_contract = TelemetryValue::Measured("editor-mcp-contract-v1".to_owned());
        harness.permission_profile =
            TelemetryValue::Measured("benchmark-agent-readwrite-v1".to_owned());
        BenchmarkRuntimeIdentity::coding_agent(
            harness,
            BenchmarkAgentRuntimeIdentity {
                runtime_id: agent_name.to_owned(),
                runtime_version: TelemetryValue::Unavailable,
            },
        )
    }

    #[test]
    fn schema_v1_through_v3_records_remain_unclassified_legacy_records() {
        for schema_version in 1..=3 {
            let mut legacy = record("legacy-model", BENCHMARK_TASKS[0], 10);
            legacy.schema_version = schema_version;
            legacy.identity.runtime = None;
            legacy.identity.execution = None;
            let bytes = serde_json::to_vec(&legacy).expect("legacy JSON");
            let loaded: BenchmarkRecord = serde_json::from_slice(&bytes).expect("legacy record");
            assert!(validate_record(&loaded).is_ok());
            assert_eq!(loaded.identity.runtime, None);
        }
    }

    #[test]
    fn acp_agent_record_keeps_unobservable_model_telemetry_unavailable() {
        let (run, directories) = failing_run_with_exchanges("acp-unavailable-model-metrics", 0);
        let runtime = agent_harness_identity("goose");
        let record = agent_run_record_with_runtime(
            "project_inspection_v1",
            &run,
            AgentRunBenchmarkIdentity {
                backend_id: "external-agent",
                model_id: "agent-managed-model",
                inventory: None,
                quality: QualityPreference::Balanced,
                workload: InferenceWorkload::InteractiveReasoning,
                hardware: &BenchmarkHardwareIdentity::default(),
            },
            runtime,
            BenchmarkModelTelemetry::unavailable(),
        )
        .expect("ACP benchmark record");
        assert_eq!(record.metrics.model_turns, TelemetryValue::Unavailable);
        assert_eq!(record.metrics.prompt_tokens, TelemetryValue::Unavailable);
        assert_eq!(record.metrics.response_tokens, TelemetryValue::Unavailable);
        assert_eq!(
            record.identity.runtime.as_ref().map(|runtime| runtime.lane),
            Some(BenchmarkLane::AgentHarness)
        );
        for directory in directories {
            let _ = std::fs::remove_dir_all(directory);
        }
    }

    #[test]
    fn raw_model_task_registers_without_reusing_agent_corpus_semantics() {
        let identity = measured_identity("model-a");
        let raw = raw_model_record(
            identity.model,
            identity.hardware,
            identity.quality,
            InferenceWorkload::InteractiveReasoning,
            BenchmarkHarnessIdentity::new("llama-direct", "raw-model-harness-v1"),
            measured_metrics(10),
        )
        .expect("raw model record");
        assert_eq!(raw.identity.task_id, RAW_MODEL_BENCHMARK_TASK_ID);
        assert_eq!(
            raw.identity.runtime.as_ref().map(|runtime| runtime.lane),
            Some(BenchmarkLane::RawModel)
        );

        let mut relabelled_agent_task = record("model-a", BENCHMARK_TASKS[0], 10);
        relabelled_agent_task.identity.runtime = raw.identity.runtime.clone();
        assert!(validate_record(&relabelled_agent_task).is_err());
    }

    #[test]
    fn benchmark_lanes_have_distinct_comparison_equivalence() {
        let raw_record = |model: &str| {
            let identity = measured_identity(model);
            raw_model_record(
                identity.model,
                identity.hardware,
                identity.quality,
                InferenceWorkload::InteractiveReasoning,
                BenchmarkHarnessIdentity::new("llama-direct", "runtime-v1"),
                measured_metrics(10),
            )
            .expect("raw model record")
        };
        let raw_left = raw_record("model-a");
        let raw_right = raw_record("model-b");
        assert_eq!(
            comparison_equivalence(&raw_left, &raw_right),
            ComparisonEquivalence::EquivalentModelComparison
        );

        let mut harness_left = record("model-a", BENCHMARK_TASKS[0], 10);
        let mut harness_right = harness_left.clone();
        harness_left.identity.runtime = Some(agent_harness_identity("goose-a"));
        harness_right.identity.runtime = Some(agent_harness_identity("goose-b"));
        assert_eq!(
            comparison_equivalence(&harness_left, &harness_right),
            ComparisonEquivalence::EquivalentAgentHarnessComparison
        );

        let mut coding_left = record("model-a", BENCHMARK_TASKS[0], 10);
        let mut coding_right = record("model-b", BENCHMARK_TASKS[0], 10);
        coding_left.identity.runtime = Some(coding_agent_identity("codex"));
        coding_right.identity.runtime = Some(coding_agent_identity("claude"));
        assert_eq!(
            comparison_equivalence(&coding_left, &coding_right),
            ComparisonEquivalence::EquivalentCodingAgentComparison
        );

        assert!(matches!(
            comparison_equivalence(&raw_left, &harness_left),
            ComparisonEquivalence::NonEquivalent(fields) if fields.contains(&"benchmark_lane")
        ));
    }

    #[test]
    fn acp_runtime_identity_match_is_exact_and_fail_closed() {
        let expected_identity =
            crate::acp_agent_runtime::AcpRuntimeIdentity::stable("goose", Some("1.2.3".to_owned()));
        let runtime = BenchmarkRuntimeIdentity::gameengine_acp_agent_harness(&expected_identity);
        assert!(runtime.matches_acp_runtime(&expected_identity));
        assert!(!runtime.matches_acp_runtime(
            &crate::acp_agent_runtime::AcpRuntimeIdentity::stable(
                "goose",
                Some("1.2.4".to_owned()),
            )
        ));
        assert!(!runtime.matches_acp_runtime(
            &crate::acp_agent_runtime::AcpRuntimeIdentity::stable(
                "another-agent",
                Some("1.2.3".to_owned()),
            )
        ));
    }

    fn measured_identity(model: &str) -> BenchmarkIdentity {
        BenchmarkIdentity {
            corpus_version: BENCHMARK_CORPUS_VERSION.to_owned(),
            task_id: BENCHMARK_TASKS[0].id.to_owned(),
            harness_version: BENCHMARK_HARNESS_VERSION.to_owned(),
            runtime_harness_version: "runtime-harness-v1".to_owned(),
            runtime: None,
            model: BenchmarkModelIdentity {
                backend_id: "test-backend".to_owned(),
                model_id: model.to_owned(),
                model_version: TelemetryValue::Measured(format!("{model}-digest")),
                quantization: TelemetryValue::Measured("q4".to_owned()),
                representation_size_bytes: TelemetryValue::Measured(1_000),
                backend_runtime_version: TelemetryValue::Measured("runtime-v1".to_owned()),
            },
            hardware: BenchmarkHardwareIdentity {
                platform: "test".to_owned(),
                gpu: TelemetryValue::Measured("gpu".to_owned()),
                total_gpu_memory_bytes: TelemetryValue::Measured(12_000),
                total_system_memory_bytes: TelemetryValue::Measured(32_000),
            },
            quality: QualityPreference::Balanced,
            workload_policy_version: WORKLOAD_POLICY_VERSION.to_owned(),
            observed_workload: TelemetryValue::Measured(InferenceWorkload::InteractiveReasoning),
            tool_budget: BenchmarkToolBudget {
                max_model_turns: 24,
                max_tool_failures: 4,
                repair_budget: 2,
                permission_budget: vec!["managed".to_owned()],
                work_claims: vec!["code_path".to_owned()],
            },
            completion_criteria: BENCHMARK_TASKS[0]
                .completion_criteria
                .iter()
                .map(|value| (*value).to_owned())
                .collect(),
            execution: None,
        }
    }

    fn measured_metrics(elapsed_ms: u64) -> BenchmarkMetrics {
        BenchmarkMetrics {
            acceptance_success: TelemetryValue::Measured(true),
            completion_success: TelemetryValue::Measured(true),
            model_turns: TelemetryValue::Measured(1),
            tool_calls: TelemetryValue::Measured(1),
            invalid_or_failed_tool_calls: TelemetryValue::Measured(0),
            code_edits: TelemetryValue::Measured(0),
            validation_attempts: TelemetryValue::Measured(0),
            repair_loops: TelemetryValue::Measured(0),
            play_attempts: TelemetryValue::Measured(0),
            frame_capture_attempts: TelemetryValue::Measured(0),
            visual_evaluation_attempts: TelemetryValue::Measured(0),
            human_interventions: TelemetryValue::Measured(0),
            elapsed_ms: TelemetryValue::Measured(elapsed_ms),
            prompt_tokens: TelemetryValue::Measured(1),
            response_tokens: TelemetryValue::Measured(1),
            load_latency_ms: TelemetryValue::Measured(1),
            ttft_ms: TelemetryValue::Unavailable,
            generation_tokens_per_second_milli: TelemetryValue::Measured(1),
            peak_backend_gpu_memory_bytes: TelemetryValue::Unavailable,
            peak_editor_gpu_memory_bytes: TelemetryValue::Unavailable,
            model_unload_reload_ms: TelemetryValue::Unavailable,
            renderer_reclaim_resume_ms: TelemetryValue::Unavailable,
            oom_failures: TelemetryValue::Measured(0),
        }
    }

    fn record(model: &str, task: BenchmarkTaskDescriptor, elapsed_ms: u64) -> BenchmarkRecord {
        let mut identity = measured_identity(model);
        identity.task_id = task.id.to_owned();
        identity.completion_criteria = task
            .completion_criteria
            .iter()
            .map(|value| (*value).to_owned())
            .collect();
        BenchmarkRecord {
            schema_version: BENCHMARK_SCHEMA_VERSION,
            recorded_unix_ms: 1,
            identity,
            metrics: measured_metrics(elapsed_ms),
        }
    }

    #[test]
    fn corpus_covers_required_first_release_workloads() {
        assert_eq!(BENCHMARK_TASKS.len(), 7);
        assert!(
            BENCHMARK_TASKS
                .iter()
                .any(|task| task.kind == BenchmarkTaskKind::ReadQuestion)
        );
        assert!(
            BENCHMARK_TASKS
                .iter()
                .any(|task| task.kind == BenchmarkTaskKind::ProjectInspection)
        );
        assert!(
            BENCHMARK_TASKS
                .iter()
                .any(|task| task.kind == BenchmarkTaskKind::CodeImplementation)
        );
        assert!(
            BENCHMARK_TASKS
                .iter()
                .any(|task| task.kind == BenchmarkTaskKind::TypedAuthoringMutation)
        );
        assert!(
            BENCHMARK_TASKS
                .iter()
                .any(|task| task.kind == BenchmarkTaskKind::ValidationRepair)
        );
        assert!(
            BENCHMARK_TASKS
                .iter()
                .any(|task| task.kind == BenchmarkTaskKind::RuntimeInteraction)
        );
        assert!(
            BENCHMARK_TASKS
                .iter()
                .any(|task| task.kind == BenchmarkTaskKind::VisualEvaluation)
        );
    }

    #[test]
    fn comparison_is_model_only_when_every_harness_dimension_matches() {
        let left = record("model-a", BENCHMARK_TASKS[0], 10);
        let right = record("model-b", BENCHMARK_TASKS[0], 10);
        assert_eq!(
            comparison_equivalence(&left, &right),
            ComparisonEquivalence::EquivalentModelComparison
        );
        let mut changed = right.clone();
        changed.identity.hardware.platform = "different".to_owned();
        assert!(
            matches!(comparison_equivalence(&left, &changed), ComparisonEquivalence::NonEquivalent(fields) if fields.contains(&"hardware"))
        );

        let mut incomplete_hardware = right.clone();
        incomplete_hardware.identity.hardware.gpu = TelemetryValue::Unavailable;
        assert!(
            matches!(comparison_equivalence(&left, &incomplete_hardware), ComparisonEquivalence::NonEquivalent(fields) if fields.contains(&"hardware"))
        );

        let mut incomplete_model = right.clone();
        incomplete_model.identity.model.representation_size_bytes = TelemetryValue::Unavailable;
        assert!(
            matches!(comparison_equivalence(&left, &incomplete_model), ComparisonEquivalence::NonEquivalent(fields) if fields.contains(&"model_representation"))
        );

        let mut different_claims = right.clone();
        different_claims.identity.tool_budget.work_claims = vec!["asset_target".to_owned()];
        assert!(
            matches!(comparison_equivalence(&left, &different_claims), ComparisonEquivalence::NonEquivalent(fields) if fields.contains(&"tool_or_permission_budget"))
        );
    }

    #[test]
    fn read_question_record_preserves_measured_hardware_snapshot() {
        let hardware = BenchmarkHardwareIdentity {
            platform: "test-platform".to_owned(),
            gpu: TelemetryValue::Measured("test-gpu".to_owned()),
            total_gpu_memory_bytes: TelemetryValue::Measured(12_000),
            total_system_memory_bytes: TelemetryValue::Measured(32_000),
        };
        let mut metrics = NativeMetrics {
            harness_version: "test-read-v1",
            backend_id: "test-backend",
            model_id: "model-a".to_owned(),
            model_turns: 1,
            retrieval_chunks: 1,
            prompt_chars: 1,
            response_chars: 1,
            elapsed_ms: 1,
            prompt_eval_tokens: Some(1),
            response_tokens: Some(1),
            backend_duration_ms: Some(1),
            load_latency_ms: Some(1),
            prompt_eval_duration_ms: Some(1),
            generation_duration_ms: Some(1),
            generation_tokens_per_second_milli: Some(1),
            ttft_ms: None,
        };
        let record = read_question_record(
            BENCHMARK_TASKS[0].id,
            &metrics,
            None,
            QualityPreference::Balanced,
            InferenceWorkload::InteractiveReasoning,
            &hardware,
        )
        .expect("read benchmark record");
        assert_eq!(record.identity.hardware, hardware);

        metrics.retrieval_chunks = 0;
        let no_provenance = read_question_record(
            BENCHMARK_TASKS[0].id,
            &metrics,
            None,
            QualityPreference::Balanced,
            InferenceWorkload::InteractiveReasoning,
            &hardware,
        )
        .expect("read benchmark record without provenance");
        assert_eq!(
            no_provenance.metrics.completion_success,
            TelemetryValue::Measured(false)
        );
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn windows_adapter_memory_is_accepted_only_when_every_match_agrees() {
        let candidates = vec![
            WindowsAdapterMemory {
                name: "GPU A".to_owned(),
                vendor_id: 1,
                device_id: 2,
                dedicated_video_memory_bytes: 12_000,
                software: false,
            },
            WindowsAdapterMemory {
                name: "GPU B".to_owned(),
                vendor_id: 3,
                device_id: 4,
                dedicated_video_memory_bytes: 8_000,
                software: false,
            },
        ];
        assert_eq!(
            select_adapter_memory("GPU A", 1, 2, &candidates),
            Some(12_000)
        );

        // Windows enumerates one physical adapter twice on a machine with a
        // virtual display driver. The duplicates describe the same card, so the
        // reading is unambiguous even though the match is not unique.
        let duplicated = vec![candidates[0].clone(), candidates[0].clone()];
        assert_eq!(
            select_adapter_memory("GPU A", 1, 2, &duplicated),
            Some(12_000)
        );

        // Two candidates that disagree are genuinely ambiguous and must not be
        // reconciled into a guessed value.
        let disagreeing = vec![
            candidates[0].clone(),
            WindowsAdapterMemory {
                dedicated_video_memory_bytes: 6_000,
                ..candidates[0].clone()
            },
        ];
        assert_eq!(select_adapter_memory("GPU A", 1, 2, &disagreeing), None);

        // A card that reports no dedicated memory is still no measurement.
        let zero = vec![WindowsAdapterMemory {
            dedicated_video_memory_bytes: 0,
            ..candidates[0].clone()
        }];
        assert_eq!(select_adapter_memory("GPU A", 1, 2, &zero), None);

        // Nothing matching at all stays unavailable.
        assert_eq!(select_adapter_memory("GPU Z", 9, 9, &candidates), None);
    }

    #[test]
    fn task_completion_requires_required_gates_to_pass() {
        let mut report = CompletionReport {
            acceptance_criteria: CompletionStatus::NotApplicable,
            authoring_validation: CompletionStatus::NotApplicable,
            source_validation: CompletionStatus::NotApplicable,
            play_launch: CompletionStatus::NotApplicable,
            frame_capture: CompletionStatus::NotApplicable,
            visual_evaluation: CompletionStatus::NotApplicable,
            interaction_scenarios: CompletionStatus::NotApplicable,
        };
        let runtime = &BENCHMARK_TASKS[5];
        assert!(!completion_report_satisfies_task(runtime, &report, 0));
        report.play_launch = CompletionStatus::Passed;
        report.interaction_scenarios = CompletionStatus::Passed;
        assert!(completion_report_satisfies_task(runtime, &report, 0));

        let repair = &BENCHMARK_TASKS[4];
        report.acceptance_criteria = CompletionStatus::Passed;
        report.source_validation = CompletionStatus::Passed;
        assert!(!completion_report_satisfies_task(repair, &report, 1));
        assert!(completion_report_satisfies_task(repair, &report, 2));
    }

    #[test]
    fn unavailable_telemetry_serializes_explicitly() {
        let value =
            serde_json::to_string(&TelemetryValue::<u64>::Unavailable).expect("telemetry JSON");
        assert!(value.contains("unavailable"));
    }

    #[test]
    fn persisted_record_contains_no_prompt_project_path_or_credentials() {
        let root = tempfile::tempdir().expect("benchmark tempdir");
        let store = BenchmarkStore::open(root.path().to_path_buf()).expect("benchmark store");
        let benchmark = record("safe-model", BENCHMARK_TASKS[0], 10);
        let path = store.record(&benchmark).expect("record benchmark");
        let text = fs::read_to_string(path).expect("record text");
        assert!(!text.contains("secret-token"));
        assert!(!text.contains("C:\\private\\project"));
        assert!(!text.contains("conversation"));
        assert!(!text.contains("\"prompt_text\""));
        assert!(text.contains("\"prompt_tokens\""));
    }

    #[test]
    fn catalog_requires_complete_same_context_success_and_provenance() {
        let candidate = CatalogCandidate {
            backend_id: "test-backend".to_owned(),
            model_id: "model-a".to_owned(),
            model_version: "model-a-digest".to_owned(),
            quantization: "q4".to_owned(),
            source: "https://example.invalid/model".to_owned(),
            license: "test-license".to_owned(),
            transfer_size_bytes: 1,
            storage_size_bytes: 1,
            memory_guidance: "test".to_owned(),
            context_limit: Some(1),
            modalities: vec!["text".to_owned()],
            tool_capabilities: vec!["structured".to_owned()],
        };
        let partial = vec![record("model-a", BENCHMARK_TASKS[0], 10)];
        let manifest = CatalogManifest {
            schema_version: CATALOG_SCHEMA_VERSION,
            catalog_version: "test".to_owned(),
            entries: vec![candidate.clone()],
        };
        let catalog =
            CuratedModelCatalog::derive(manifest.clone(), &partial).expect("partial catalog");
        assert!(catalog.recommendation(CatalogProfile::Balanced).is_none());

        let complete = BENCHMARK_TASKS
            .iter()
            .copied()
            .map(|task| record("model-a", task, 10))
            .collect::<Vec<_>>();
        let mut incomplete_identity = complete.clone();
        for record in &mut incomplete_identity {
            record.identity.hardware.gpu = TelemetryValue::Unavailable;
        }
        let incomplete_catalog =
            CuratedModelCatalog::derive(manifest.clone(), &incomplete_identity)
                .expect("incomplete identity catalog");
        assert!(
            incomplete_catalog
                .recommendation(CatalogProfile::Balanced)
                .is_none()
        );

        let catalog = CuratedModelCatalog::derive(manifest, &complete).expect("complete catalog");
        let recommendation = catalog
            .recommendation(CatalogProfile::Balanced)
            .expect("balanced recommendation");
        assert_eq!(recommendation.candidate.model_id, candidate.model_id);
        assert_eq!(recommendation.evidence_runs, BENCHMARK_TASKS.len());
    }
}
