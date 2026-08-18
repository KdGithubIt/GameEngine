# ADR 0155: GameEngine-Managed Local Inference Runtime and Platform Selection

Status: Accepted
Date: 2026-08-18
Builds on: ADR 0131, ADR 0135, ADR 0143
Relates to: ADR 0142, ADR 0144, ADR 0150, ADR 0153

## Context

ADR 0131 defines a provider-independent native Agent Runtime with local and hosted
`ModelBackend` implementations. ADR 0135 defines application-layer inference
resource arbitration, and ADR 0143 requires truthful backend resource controls
and telemetry. The first local implementation is an Ollama-compatible loopback
adapter. That is useful for initial bring-up, but it leaves installation, model
registration, runtime versioning, process lifecycle, and most performance tuning
outside GameEngine.

For a production Editor, requiring every user to install and operate a separate
local-model application is a poor ownership boundary. It also makes
benchmarking harder because two machines can select the same model name while
using different runtime builds, platform paths, launch options, and residency
behavior.

`llama.cpp` is a suitable initial managed runtime because it can serve GGUF
models on Windows and Linux and exposes enough execution controls for the ADR
0135/0143 resource policy. The architecture must still avoid turning one
inference implementation into a permanent public API. Future local runtimes may
be added when they satisfy the same lifecycle, security, telemetry, and
benchmark identity contracts.

Windows creates an additional deployment choice. Native Windows inference has
the lowest setup cost. WSL2 can run the Linux runtime while the Editor remains a
Windows application and may have different performance or memory behavior.
Native Linux remains relevant for Linux hosts and as a reference environment.
The product must not assume that one environment is always faster. Selection
must be based on measured behavior and user policy rather than folklore.

A GameEngine-managed WSL2 path is feasible, but the first machine setup can
cross an operating-system privilege boundary. Enabling required Windows
features may require elevation and a reboot. GameEngine can orchestrate that
flow, but it cannot silently bypass UAC, force a reboot without consent, or
pretend that a missing or incompatible GPU driver is an engine-owned model
failure.

A project-wide decision is therefore required for managed local-runtime
ownership, platform selection, WSL2 provisioning, model acquisition and
storage, runtime provenance, loopback transport, lifecycle recovery, migration
from the external Ollama-compatible path, and benchmark identity.

## Decision

### 1. GameEngine owns a first-class managed local inference service

The preferred local-native AI product path is a GameEngine-managed inference
service rather than a requirement that the user separately install and operate
a local-model application.

Conceptually:

```text
AI Studio / Native Agent Runtime
              |
        ModelBackend
              |
   ManagedLocalRuntime
      /      |       \
 Windows   WSL2    native Linux
      \      |       /
         llama.cpp
```

The managed service owns the application-level lifecycle required to make a
local model usable:

- runtime installation and version selection;
- runtime health probing;
- model registration and content identity;
- process start, stop, restart, and idle shutdown;
- safe model load, unload, and residency requests;
- platform-specific launch configuration;
- loopback transport establishment;
- runtime and model provenance used by benchmark records; and
- actionable diagnostics when setup or execution cannot proceed.

AI Studio remains a frontend over Agent Host and ModelBackend contracts. It MUST
NOT own provider-specific process management in GUI code.

### 2. `llama.cpp` is the initial managed runtime, not a permanent public ABI

The first managed implementation uses a pinned, known `llama.cpp` runtime and
its server-capable execution path.

Provider-specific command-line flags, HTTP paths, process output, runtime
directory layout, and internal model-load behavior remain private to the
managed adapter. The rest of the Agent Runtime communicates through the
provider-independent ModelBackend/resource contracts established by ADR 0131
and ADR 0143.

The backend identity for managed `llama.cpp` evidence MUST remain distinct from
the existing `ollama-compatible` identity. Existing benchmark records are never
relabelled as managed-runtime evidence.

A future local runtime MAY coexist with or replace `llama.cpp` only through the
same provider-independent boundary and with equivalent lifecycle, provenance,
security, and observability guarantees.

### 3. Managed local execution supports explicit platform modes

The managed local-runtime layer recognizes at least these execution
environments when supported by the host:

```text
WindowsNative
Wsl2Linux
NativeLinux
```

These are execution-environment identities, not model identities.

On Windows, AI Studio SHOULD expose a simple runtime preference:

```text
Auto
Windows native
WSL2
```

`Auto` selects only among environments that have passed local capability and
health checks. It MUST NOT assume that WSL2 or Windows is universally faster.

When both Windows native and WSL2 are eligible, GameEngine MAY perform a short
local calibration or consume equivalent ADR 0156 runtime-characterization
evidence. The selected environment and the evidence behind the decision are
machine-local product state. A user may override Auto.

A Linux-hosted Editor uses `NativeLinux`; this ADR does not require dual boot or
native Linux installation on a Windows machine.

### 4. WSL2 is engine-managed when selected

When WSL2 is used, GameEngine SHOULD provision a dedicated managed environment
instead of installing runtime files into an arbitrary user-owned Linux
distribution.

Conceptually the distribution is an implementation-owned environment such as:

```text
GameEngine-LocalAI
  runtime/
  models/
  cache/
  state/
```

The exact distribution name and filesystem layout are not serialized product
contracts.

The managed WSL environment MUST:

- be dedicated to GameEngine local inference;
- avoid modifying unrelated user distributions;
- keep runtime and model state separate from canonical project data;
- support complete removal from the GameEngine local-AI settings flow;
- expose only the minimum host integration required for inference; and
- keep model/runtime files in a Linux-native filesystem when WSL execution uses
  them for performance-sensitive access.

If switching between Windows-native and WSL execution would duplicate a
multi-gigabyte model representation, GameEngine MUST show the additional
storage requirement before copying it. Content identity MUST remain tied to the
same model bytes even when platform-local caches contain separate copies.

### 5. Privilege elevation and reboot are explicit operating-system boundaries

GameEngine may automate WSL2 enablement and managed-distribution provisioning,
but operating-system privilege transitions remain explicit.

If required Windows components are unavailable, the setup flow may request
elevation to enable them. The UI MUST explain why elevation is needed before
invoking the operating-system prompt.

GameEngine MUST NOT:

- silently bypass UAC;
- treat a denied elevation as model incompatibility;
- reboot the machine without explicit user consent; or
- claim that a setup requiring a reboot completed before the reboot and
  continuation check succeed.

If setup requires restart, GameEngine SHOULD persist a machine-local
continuation marker so the Local AI setup can resume after the user reopens the
application.

Supported GPU drivers remain platform prerequisites. GameEngine SHOULD diagnose
missing or incompatible driver capability, but vendor-driver installation is
not owned by the model runtime adapter unless a later ADR explicitly adopts
that responsibility.

### 6. Runtime artifacts are pinned, verified, and rollback-capable

GameEngine-managed runtime artifacts MUST have explicit provenance.

A managed runtime installation records at least:

- runtime family;
- exact runtime version or source revision;
- target platform/backend variant;
- expected artifact digest;
- installation timestamp or installed product version; and
- any runtime compatibility version needed by ModelBackend.

Runtime downloads MUST be integrity-checked before activation. A failed update
MUST NOT destroy the last known usable managed runtime. The runtime manager
SHOULD retain or reconstruct one rollback target when practical.

Runtime updates are independent from model-weight updates. Updating GameEngine
MUST NOT silently reinterpret an old model benchmark as evidence from a new
runtime build.

### 7. Model acquisition is explicit and provenance-preserving

ADR 0142 remains authoritative that model weights are not silently downloaded
merely because a model is recommended.

The managed Local AI flow MAY automate model acquisition after one explicit user
action that shows, when known:

- source;
- model/representation identity;
- quantization or equivalent representation;
- license/provenance;
- expected transfer size; and
- expected storage requirement.

Downloads SHOULD be resumable and content-verified.

Advanced users may register an existing compatible GGUF file without an Ollama
`Modelfile` or provider-specific import step. Registration MUST NOT alter the
model bytes. A content digest or equivalent immutable representation identity
SHOULD be retained so the same file can be recognized across campaigns and
platform-local caches.

Model files, runtime binaries, caches, and installation state are application
data and MUST NOT be written into canonical GameEngine project authoring data or
packaged game content.

### 8. The managed inference transport remains local and non-authoritative

The managed runtime is a local implementation detail. It does not gain project
authoring authority.

A managed server or proxy MUST be reachable by GameEngine through a loopback or
equivalently host-confined transport. It MUST NOT be exposed to the LAN merely
to make Windows-to-WSL communication convenient.

The Windows-to-WSL adapter may use supported local forwarding or a host-side
proxy, but the Editor-facing connection still obeys the local-only trust
boundary. Remote AI Studio from ADR 0133 is a separate authenticated surface and
MUST NOT be implemented by exposing the model server.

Model inference never bypasses Agent Host permissions, the code workspace,
MCP/authoring stale-state checks, managed validation, or completion gates.

### 9. Runtime lifecycle is demand-driven and recoverable

GameEngine starts the managed local runtime only when needed or when an explicit
user preference requests warm residency.

Before inference, the manager verifies:

1. the selected environment is still available;
2. the pinned runtime installation is intact;
3. the selected model representation is present;
4. the managed endpoint/process becomes healthy; and
5. the backend reports the capabilities required by the requested operation.

Idle shutdown, model unload, process shutdown, and restart policy are
application concerns. They MUST preserve ADR 0135 interruption/resume semantics
and ADR 0143 evidence requirements.

A crashed runtime may be restarted automatically only when doing so does not
misrepresent a partially executed model turn as successful. In-flight inference
failure remains observable to the Agent Runtime and benchmark record.

### 10. Resource policy remains owned by ADR 0135/0143

The managed runtime exposes truthful capabilities; it does not invent a second
GPU scheduler.

Supported launch or runtime controls may include context size, batch sizing,
CPU/GPU offload, device selection, KV-cache placement, Flash Attention,
residency, unload/reload, and runtime-specific optimization features. These are
backend mechanisms consumed by the existing application-layer resource policy.

AI Studio SHOULD continue to present user-level quality/resource intent instead
of requiring ordinary users to understand provider flags. Advanced diagnostic
or benchmark surfaces MAY expose the resolved launch plan.

If a requested control is unsupported in one execution environment, it remains
unavailable. GameEngine MUST NOT report resource parity merely because another
environment supports the feature.

### 11. Platform and runtime identity are first-class benchmark dimensions

ADR 0142 model-only comparison remains strict.

Benchmark evidence for a managed local model records enough runtime identity to
distinguish at least:

- managed backend family;
- exact runtime version/revision;
- execution environment (`WindowsNative`, `Wsl2Linux`, or `NativeLinux`);
- model representation identity and quantization;
- GameEngine benchmark/harness versions; and
- hardware identity required by ADR 0142.

A Windows-native run and a WSL2 run are NOT equivalent model-only evidence even
when they use identical GGUF bytes. They are runtime/platform characterization
evidence and must be labelled accordingly.

Likewise, changing runtime revision, execution environment, or material launch
policy cannot be hidden inside a model ranking.

### 12. Existing Ollama compatibility remains an explicit migration path

The existing Ollama-compatible backend remains supported as an external
compatibility path during migration. It is not silently reinterpreted as the
managed runtime.

Existing AI Studio preferences, custom endpoint values, and
`ollama-compatible` benchmark records retain their meaning. A settings migration
MUST be additive and MUST NOT change a stored external endpoint into a managed
runtime selection without user action.

Where the original GGUF file is available, GameEngine may offer to register or
copy it into managed storage. GameEngine MUST NOT depend on undocumented Ollama
storage internals or scrape private provider state to claim migration success.

Removing Ollama compatibility entirely requires a separate compatibility
decision after managed local inference is proven in normal use.

### 13. First-release user experience is setup-oriented, not terminal-oriented

The first managed Local AI experience SHOULD make the normal path:

```text
Open AI Studio
  -> Set up Local AI
  -> choose Auto / Windows / WSL2
  -> satisfy any one-time OS prerequisite
  -> choose or register a model
  -> explicit download/import
  -> Ready
```

Ordinary use MUST NOT require the user to:

- install Ollama;
- write an Ollama `Modelfile`;
- run `ollama create`;
- compile `llama.cpp`;
- install a compiler toolchain;
- type a `llama-server` command;
- discover or enter an internal localhost port; or
- manually launch the managed inference process.

Advanced users may still select an external compatible backend.

### 14. Failure diagnostics distinguish setup, runtime, model, and resource layers

Managed Local AI errors MUST identify the responsible layer where possible.

At minimum diagnostics distinguish:

- operating-system prerequisite unavailable;
- elevation denied or restart required;
- WSL distribution provisioning failure;
- GPU/backend capability unavailable;
- runtime artifact integrity/update failure;
- model transfer or content verification failure;
- managed process/server startup failure;
- model load or OOM/resource failure; and
- inference protocol/model-turn failure.

The product MUST NOT respond to an environment failure by silently changing
model bytes, benchmark identity, project data, or security posture.

## Implementation

The first-release implementation may keep the existing Ollama-compatible
adapter while adding a new managed-local adapter and runtime manager. The
preferred UI path becomes managed Local AI after its setup, lifecycle,
resource-control, benchmark-identity, and recovery behavior are validated.

Windows native and WSL2 use the same provider-independent ModelBackend contract.
Platform-specific launchers remain application-layer adapters. Linux-hosted
GameEngine reuses the Linux managed runtime without introducing a separate Agent
Runtime.

Machine-local runtime state may include installed runtime versions, environment
selection, managed WSL identity, model-cache locations, calibration results,
health state, and update metadata. None of this state is canonical project data.

## Verification

Deterministic tests and platform validation must cover:

- managed runtime installation metadata and digest rejection;
- no silent model acquisition;
- existing-GGUF registration without byte mutation;
- Windows-native launch and health lifecycle;
- WSL2 capability detection and fail-closed setup behavior;
- explicit elevation/restart continuation state without bypassing OS consent;
- managed distribution isolation from unrelated user distributions;
- loopback-only Editor-facing transport;
- process crash/restart behavior without false successful turns;
- model load/unload integration with ADR 0135/0143;
- preserved Agent Host permission and completion boundaries;
- distinct benchmark identity for Windows-native, WSL2, native Linux, and
  Ollama-compatible evidence;
- settings migration preserving old external endpoint meaning; and
- runtime update failure preserving a usable prior installation when rollback is
  supported.

Windows and WSL2 performance claims require measured runtime-characterization
evidence rather than deterministic tests.

Any new Local AI setup, runtime-selection, download, model-management, progress,
or diagnostics UI requires Editor Visual Validation.

## Non-goals

This ADR does not:

- require native Linux installation on Windows;
- declare WSL2 universally faster than Windows native;
- select one permanent model family or quantization;
- bundle model weights without explicit acquisition consent;
- turn `llama.cpp` command-line flags into GameEngine public APIs;
- expose the local model server remotely;
- make GameEngine responsible for arbitrary vendor GPU-driver installation; or
- remove the existing Ollama-compatible path without a later compatibility
  decision.
