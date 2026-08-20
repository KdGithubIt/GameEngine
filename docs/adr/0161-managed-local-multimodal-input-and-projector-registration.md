# ADR 0161: Managed Local Multimodal Input and Projector Registration

Status: Accepted
Date: 2026-08-20
Builds on: ADR 0131, ADR 0142, ADR 0150, ADR 0155
Relates to: ADR 0157, ADR 0159

## Context

ADR 0131 makes visual evaluation part of the completion contract: a run that
claims a visual result must have looked at a host-captured frame. ADR 0142 makes
`visual_evaluation_v1` one of the benchmark task classes, and ADR 0150 refuses to
route a workload that requires image input to a model that does not declare or
demonstrate it.

The managed local runtime from ADR 0155 could not participate. Its adapter
refused image input outright and its capability profile reported
`image_input: Some(false)` for every registered model, so the one backend
GameEngine installs and pins was structurally excluded from the visual half of
its own contract. The Ollama-compatible adapter accepts images, but that backend
is the user's own installation rather than the runtime GameEngine manages.

The underlying reason is mechanical rather than architectural. A GGUF text model
cannot read an image on its own; it needs a separate multimodal projector file,
and the server must be launched with that projector. Which projector belongs to
which model is a fact about a pair of files, not a property that can be derived
from a model's name, and inferring it from names is exactly the model-family
special-casing the project has avoided elsewhere.

## Decision

### 1. Image capability comes from a registered projector, never from a name

A managed model registration MAY carry one **multimodal projector**: a second
GGUF file, registered against a model that is already registered, recorded with
its own content digest, byte size, and modification time.

Nothing about this is specific to a model family. Any projector registered
against any model makes that configuration image-capable, and a model with no
projector stays text-only. GameEngine MUST NOT infer image capability, projector
identity, or projector location from a model's file name, display name, or
architecture string.

### 2. The capability profile reports the configuration, not a guess

`image_input` for a managed configuration is `Some(true)` exactly when that
configuration resolved a projector, and `Some(false)` otherwise. It is never
`None`, because the answer is always known: the registry either has a projector
for the model or it does not.

An image request against a configuration with no projector is refused with a
capability diagnostic before any request is issued, rather than being sent and
failing inside the runtime.

### 3. The projector is part of model preparation and integrity

A projector is prepared for its execution environment exactly like the model it
belongs to: verified in place for Windows-native execution, and copied into the
managed WSL2 distribution with a digest check for WSL2 execution. Preparing a
model prepares its projector in the same operation, so a prepared model is never
left unable to answer the first image request.

The WSL2 copy is the same content-addressed transfer the model uses: the file
lands only after its digest matches inside the distribution, and the additional
storage it needs is subject to the same explicit approval.

### 4. Images travel as request content, never as a file path

Image bytes are sent in the request as data URLs in an OpenAI-compatible content
array. The runtime is loopback-only, so the bytes do not leave the machine, and
the adapter does not ask the runtime to open a path that may not exist inside its
execution environment.

A text-only turn keeps exactly the request shape it had before this ADR, so
adding image support does not change text behaviour or its benchmark identity.

## Verification

- a registered projector reaches the launch command in both execution
  environments, and its absence adds no launch argument;
- `image_input` follows the resolved projector in the capability profile;
- an image request without a projector is refused with a capability diagnostic;
- registering and removing a projector changes only the targeted model;
- registering a projector for an unknown model is refused; and
- the WSL2 projector copy is verified by digest before it is used.

## Consequences

The managed local runtime can take part in visual evaluation, which is what ADR
0131 completion and ADR 0142 benchmarking already require of any backend that
claims a visual result.

Model registration gains an optional record, so the machine-local registry format
changes additively. Registries written before this ADR carry no projector and
keep behaving exactly as before.

Users must obtain and register the projector that matches their model. That is a
deliberate cost: it is the only way to pair the files without guessing, and the
guess would be wrong for every model family the project has not seen.

## Alternatives Considered

**Infer the projector from the model file name.** Rejected. Naming conventions
differ per publisher and per quantization, and a wrong pairing produces a runtime
that starts and then answers nonsense.

**Bundle a projector with the pinned runtime.** Rejected. A projector is tied to
a model, not to a llama.cpp revision, so bundling one would only work for the
models it happened to match.

**Leave image input to the Ollama-compatible backend.** Rejected. That backend is
the user's own installation; the managed runtime is the one GameEngine installs,
pins, and benchmarks, and excluding it from visual evaluation would exclude the
default path from half of its own completion contract.

## Compatibility and Migration

The projector record is additive and optional in the machine-local managed model
registry. No project document, scene, benchmark identity field, or session format
changes. A benchmark record produced with a projector configured is comparable to
one produced without it for every metric that does not involve image input, and
`image_input` in the capability profile states which case a record came from.
