//! Provider-independent native inference scheduling and resource arbitration.
//!
//! This module intentionally owns no renderer or model-provider implementation.
//! It describes application-layer workload, quality, capability, reclaim, and
//! benchmark contracts so AI Studio can coordinate those owners without leaking
//! agent semantics into low-level rendering.

use serde::{Deserialize, Serialize};

/// Resource-relevant workload classification for native agent work.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum InferenceWorkload {
    /// Low-latency model reasoning for conversation and lightweight inspection.
    InteractiveReasoning,
    /// Higher-value reasoning where implementation decisions should be retained.
    StrongReasoning,
    /// A fully specified managed operation that does not currently need inference.
    DeterministicExecution,
    /// Live Play, frame capture, or other runtime observation work.
    RuntimeObservation,
}

/// User intent for native reasoning latency versus quality.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum QualityPreference {
    /// Let the harness choose the least expensive policy expected to succeed.
    #[default]
    Auto,
    /// Prefer responsive inference and avoid expensive passes.
    Fast,
    /// Allow normal implementation and repair reasoning.
    Balanced,
    /// Prefer the strongest supported reasoning/resource posture.
    Deep,
}

impl QualityPreference {
    pub(crate) const ALL: [Self; 4] = [Self::Auto, Self::Fast, Self::Balanced, Self::Deep];

    pub(crate) const fn label(self) -> &'static str {
        match self {
            Self::Auto => "Auto",
            Self::Fast => "Fast",
            Self::Balanced => "Balanced",
            Self::Deep => "Deep",
        }
    }
}

/// Inputs used to classify work independently from an `AgentRun` phase.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct WorkloadSignals {
    /// The next operation is already fully specified and managed by the engine.
    pub(crate) fully_specified_managed_operation: bool,
    /// Correctness requires live runtime/render observation.
    pub(crate) runtime_observation_required: bool,
    /// Current evidence requires a stronger architecture/repair reasoning pass.
    pub(crate) strong_reasoning_required: bool,
    /// A tool result still requires model judgement before the next operation.
    pub(crate) model_judgement_required: bool,
}

/// Classifies work without treating semantic run phase as a GPU scheduler API.
pub(crate) fn classify_workload(signals: WorkloadSignals) -> InferenceWorkload {
    if signals.runtime_observation_required {
        InferenceWorkload::RuntimeObservation
    } else if signals.fully_specified_managed_operation && !signals.model_judgement_required {
        InferenceWorkload::DeterministicExecution
    } else if signals.strong_reasoning_required {
        InferenceWorkload::StrongReasoning
    } else {
        InferenceWorkload::InteractiveReasoning
    }
}

/// Whether a provider-independent backend operation is actually exposed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum CapabilityAvailability {
    /// The backend exposes a governed implementation of this operation.
    Available,
    /// The backend does not expose this operation.
    Unavailable,
}

/// Resource controls a `ModelBackend` may expose.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct ModelResourceCapabilities {
    pub(crate) representation_size: CapabilityAvailability,
    pub(crate) gpu_residency: CapabilityAvailability,
    pub(crate) cpu_gpu_offload: CapabilityAvailability,
    pub(crate) selectable_device: CapabilityAvailability,
    pub(crate) kv_context_placement: CapabilityAvailability,
    pub(crate) inference_cache_release: CapabilityAvailability,
    pub(crate) unload_reload: CapabilityAvailability,
    pub(crate) backend_memory_telemetry: CapabilityAvailability,
}

impl Default for ModelResourceCapabilities {
    fn default() -> Self {
        Self {
            representation_size: CapabilityAvailability::Unavailable,
            gpu_residency: CapabilityAvailability::Unavailable,
            cpu_gpu_offload: CapabilityAvailability::Unavailable,
            selectable_device: CapabilityAvailability::Unavailable,
            kv_context_placement: CapabilityAvailability::Unavailable,
            inference_cache_release: CapabilityAvailability::Unavailable,
            unload_reload: CapabilityAvailability::Unavailable,
            backend_memory_telemetry: CapabilityAvailability::Unavailable,
        }
    }
}

/// Resource telemetry provenance. Unknown values remain explicitly unavailable.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "availability", content = "value", rename_all = "snake_case")]
pub(crate) enum TelemetryValue<T> {
    /// Value reported or measured by the owning subsystem.
    Measured(T),
    /// Conservative estimate used because exact measurement is unavailable.
    ConservativeEstimate(T),
    /// The owning subsystem cannot determine this value reliably.
    Unavailable,
}

impl<T> Default for TelemetryValue<T> {
    fn default() -> Self {
        Self::Unavailable
    }
}

/// Observed pressure used by the application-layer broker.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum MemoryPressure {
    /// Available telemetry indicates normal headroom.
    Normal,
    /// Exact free memory is unavailable, so policy must retain conservative headroom.
    Unknown,
    /// Reliable evidence indicates shared-GPU pressure.
    Constrained,
}

/// How aggressively Editor-owned recreatable GPU resources may be reclaimed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ReclaimLevel {
    /// Keep normal presentation resources resident.
    None,
    /// Release view-local/transient targets while keeping reusable caches.
    Transient,
    /// Release additional recreatable reusable residency after transient reclaim.
    Aggressive,
}

/// Which subsystem receives shared-GPU priority for the next step.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ResourcePriority {
    /// Normal interactive Editor operation.
    EditorInteractive,
    /// Native inference receives priority while optional presentation is suspended.
    NativeInference,
    /// Play/render/frame capture receives priority over local model residency.
    RuntimeRendering,
}

/// Model residency request expressed without provider-specific knobs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ModelResidencyRequest {
    /// Keep the current backend residency posture.
    Keep,
    /// Reduce residency only when the backend exposes an explicit supported control.
    ReduceIfSupported,
    /// Unload/release residency only when the backend exposes an explicit supported control.
    ReleaseIfSupported,
}

/// Presentation posture selected by the broker.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum PresentationPosture {
    /// Normal Editor presentation.
    Interactive,
    /// Optional GPU-heavy presentation work is suspended.
    InferenceFocused,
}

/// Provider-independent result of one arbitration decision.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ResourcePlan {
    pub(crate) workload: InferenceWorkload,
    pub(crate) priority: ResourcePriority,
    pub(crate) presentation: PresentationPosture,
    pub(crate) reclaim: ReclaimLevel,
    pub(crate) model_residency: ModelResidencyRequest,
    /// Whether the active operation should invoke a model at all.
    pub(crate) inference_required: bool,
}

/// Resolves a resource posture without hard-coded model names or VRAM thresholds.
pub(crate) fn resolve_resource_plan(
    workload: InferenceWorkload,
    quality: QualityPreference,
    pressure: MemoryPressure,
    capabilities: ModelResourceCapabilities,
) -> ResourcePlan {
    match workload {
        InferenceWorkload::DeterministicExecution => ResourcePlan {
            workload,
            priority: ResourcePriority::EditorInteractive,
            presentation: PresentationPosture::Interactive,
            reclaim: ReclaimLevel::None,
            model_residency: if capabilities.unload_reload == CapabilityAvailability::Available {
                ModelResidencyRequest::ReleaseIfSupported
            } else {
                ModelResidencyRequest::Keep
            },
            inference_required: false,
        },
        InferenceWorkload::RuntimeObservation => ResourcePlan {
            workload,
            priority: ResourcePriority::RuntimeRendering,
            presentation: PresentationPosture::Interactive,
            reclaim: ReclaimLevel::None,
            model_residency: if capabilities.unload_reload == CapabilityAvailability::Available {
                ModelResidencyRequest::ReleaseIfSupported
            } else if capabilities.gpu_residency == CapabilityAvailability::Available {
                ModelResidencyRequest::ReduceIfSupported
            } else {
                ModelResidencyRequest::Keep
            },
            inference_required: false,
        },
        InferenceWorkload::StrongReasoning => {
            let reclaim = match (quality, pressure) {
                (QualityPreference::Deep, MemoryPressure::Constrained) => ReclaimLevel::Aggressive,
                (_, MemoryPressure::Normal) if quality == QualityPreference::Fast => {
                    ReclaimLevel::None
                }
                _ => ReclaimLevel::Transient,
            };
            ResourcePlan {
                workload,
                priority: ResourcePriority::NativeInference,
                presentation: if reclaim == ReclaimLevel::None {
                    PresentationPosture::Interactive
                } else {
                    PresentationPosture::InferenceFocused
                },
                reclaim,
                model_residency: ModelResidencyRequest::Keep,
                inference_required: true,
            }
        }
        InferenceWorkload::InteractiveReasoning => ResourcePlan {
            workload,
            priority: ResourcePriority::NativeInference,
            presentation: if quality == QualityPreference::Deep
                && pressure != MemoryPressure::Normal
            {
                PresentationPosture::InferenceFocused
            } else {
                PresentationPosture::Interactive
            },
            reclaim: if quality == QualityPreference::Deep
                && pressure != MemoryPressure::Normal
            {
                ReclaimLevel::Transient
            } else {
                ReclaimLevel::None
            },
            model_residency: ModelResidencyRequest::Keep,
            inference_required: true,
        },
    }
}

/// Machine-readable benchmark record for one native inference/managed-work cycle.
///
/// Every value that cannot be measured by the owning subsystem remains
/// [`TelemetryValue::Unavailable`]; callers must not replace it with a guessed
/// precise value.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct NativeBenchmarkRecord {
    pub(crate) model_identity: String,
    pub(crate) backend_runtime: String,
    pub(crate) quality_preference: QualityPreference,
    pub(crate) resolved_workload: InferenceWorkload,
    pub(crate) hardware_identity: TelemetryValue<String>,
    pub(crate) total_gpu_memory_bytes: TelemetryValue<u64>,
    pub(crate) load_latency_ms: TelemetryValue<u64>,
    pub(crate) reload_latency_ms: TelemetryValue<u64>,
    pub(crate) unload_latency_ms: TelemetryValue<u64>,
    pub(crate) ttft_ms: TelemetryValue<u64>,
    pub(crate) generation_tokens_per_second_milli: TelemetryValue<u64>,
    pub(crate) reasoning_wall_time_ms: TelemetryValue<u64>,
    pub(crate) peak_model_gpu_memory_bytes: TelemetryValue<u64>,
    pub(crate) peak_editor_gpu_memory_bytes: TelemetryValue<u64>,
    pub(crate) reclaim_level: ReclaimLevel,
    pub(crate) editor_suspend_latency_ms: TelemetryValue<u64>,
    pub(crate) editor_resume_latency_ms: TelemetryValue<u64>,
    pub(crate) renderer_reconstruction_latency_ms: TelemetryValue<u64>,
    pub(crate) inference_oom: TelemetryValue<bool>,
    pub(crate) renderer_oom: TelemetryValue<bool>,
    pub(crate) acceptance_success: TelemetryValue<bool>,
    pub(crate) validation_attempts: u32,
    pub(crate) repair_attempts: u32,
    pub(crate) human_interruption: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn workload_classification_is_not_run_phase_mapping() {
        assert_eq!(
            classify_workload(WorkloadSignals {
                fully_specified_managed_operation: true,
                ..WorkloadSignals::default()
            }),
            InferenceWorkload::DeterministicExecution
        );
        assert_eq!(
            classify_workload(WorkloadSignals {
                fully_specified_managed_operation: true,
                model_judgement_required: true,
                strong_reasoning_required: true,
                ..WorkloadSignals::default()
            }),
            InferenceWorkload::StrongReasoning
        );
        assert_eq!(
            classify_workload(WorkloadSignals {
                runtime_observation_required: true,
                strong_reasoning_required: true,
                ..WorkloadSignals::default()
            }),
            InferenceWorkload::RuntimeObservation
        );
    }

    #[test]
    fn quality_resolution_never_forces_model_into_deterministic_execution() {
        for quality in QualityPreference::ALL {
            let plan = resolve_resource_plan(
                InferenceWorkload::DeterministicExecution,
                quality,
                MemoryPressure::Unknown,
                ModelResourceCapabilities::default(),
            );
            assert!(!plan.inference_required);
            assert_eq!(plan.presentation, PresentationPosture::Interactive);
        }
    }

    #[test]
    fn unknown_memory_uses_conservative_transient_reclaim_not_fabricated_capacity() {
        let plan = resolve_resource_plan(
            InferenceWorkload::StrongReasoning,
            QualityPreference::Deep,
            MemoryPressure::Unknown,
            ModelResourceCapabilities::default(),
        );
        assert_eq!(plan.reclaim, ReclaimLevel::Transient);
        let free_memory: TelemetryValue<u64> = TelemetryValue::Unavailable;
        assert_eq!(free_memory, TelemetryValue::Unavailable);
    }

    #[test]
    fn deep_only_uses_aggressive_reclaim_with_observed_pressure() {
        let constrained = resolve_resource_plan(
            InferenceWorkload::StrongReasoning,
            QualityPreference::Deep,
            MemoryPressure::Constrained,
            ModelResourceCapabilities::default(),
        );
        assert_eq!(constrained.reclaim, ReclaimLevel::Aggressive);

        let balanced = resolve_resource_plan(
            InferenceWorkload::StrongReasoning,
            QualityPreference::Balanced,
            MemoryPressure::Constrained,
            ModelResourceCapabilities::default(),
        );
        assert_eq!(balanced.reclaim, ReclaimLevel::Transient);
    }

    #[test]
    fn runtime_observation_prioritizes_renderer_and_only_requests_supported_release() {
        let unsupported = resolve_resource_plan(
            InferenceWorkload::RuntimeObservation,
            QualityPreference::Deep,
            MemoryPressure::Constrained,
            ModelResourceCapabilities::default(),
        );
        assert_eq!(unsupported.priority, ResourcePriority::RuntimeRendering);
        assert_eq!(unsupported.model_residency, ModelResidencyRequest::Keep);

        let supported = resolve_resource_plan(
            InferenceWorkload::RuntimeObservation,
            QualityPreference::Deep,
            MemoryPressure::Constrained,
            ModelResourceCapabilities {
                unload_reload: CapabilityAvailability::Available,
                ..ModelResourceCapabilities::default()
            },
        );
        assert_eq!(
            supported.model_residency,
            ModelResidencyRequest::ReleaseIfSupported
        );
    }
}
