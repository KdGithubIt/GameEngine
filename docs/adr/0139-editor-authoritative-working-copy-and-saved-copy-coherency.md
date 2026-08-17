# ADR 0139: Editor Authoritative Working Copy and Saved-Copy Coherency

Status: Accepted
Date: 2026-08-17
Builds on: ADR 0019, ADR 0022
Relates to: ADR 0005, ADR 0008, ADR 0025, ADR 0068, ADR 0072, ADR 0085, ADR 0136, ADR 0137, ADR 0138

## Problem

GameEngine intentionally separates interactive editing from explicit disk persistence. That is correct: every slider movement or graph operation should not churn the Git working tree. The missing contract is in-process visibility. Several Editor subsystems can hold an edited in-memory document while another subsystem reloads the same asset from disk.

This creates contradictory UX:

```text
Animation Set Editor shows every Motion Slot bound
  -> its in-memory document is dirty
  -> Scene View / validation / debug reloads the older disk copy
  -> Animation Controller is reported invalid
```

The user sees one project, but the Editor behaves as if each panel owns a different truth. A specialized editor's Save button becomes an undocumented publication step instead of persistence.

`DocumentWorkspace` already keeps in-memory sessions, revisions, dirty state, undo, save-all, and recovery for several document types. Animation Set and Material editors also keep in-memory documents and undo history. ADR 0019/0022 establish explicit save and authoring documents as source of truth, but do not yet define one cross-Editor read contract. ADR 0136 separately establishes project/device-scoped immutable preview asset residency; that cache must not be confused with mutable authoring working-copy ownership.

## Decision

### 1. The Editor has one authoritative working copy per opened/edited document identity

Within one Editor project process, an opened or modified authoring document has one authoritative **working copy**. That is the state users see and the state other Editor authoring consumers must read.

Conceptually the Editor exposes a project-scoped working-copy registry/read service:

```text
Document identity
  -> current in-memory document snapshot
  -> revision/generation
  -> dirty/clean state
  -> saved baseline identity
  -> owning edit session
```

This is a semantic contract, not a requirement that every panel use one Rust struct. `DocumentWorkspace`, Animation Set Editor, Material Editor, and future specialized editors may retain domain-specific edit-session types, but they publish/read through one working-copy authority.

There MUST NOT be two independently mutable working copies for the same document identity in one Editor process.

### 2. Accepted edits are visible immediately to Editor consumers

After an edit is accepted by the owning authoring/edit session:

1. the authoritative working copy is updated;
2. its revision/generation advances;
3. dirty state is updated;
4. Inspector, Scene View, validation, Problems, Graph Debug, preview, and other Editor-side consumers resolve that working copy; and
5. disk remains unchanged until explicit persistence.

A dedicated editor's Save button or `Ctrl+S` is therefore a persistence operation, not a publication operation. The user must not need to save an Animation Graph or Animation Set merely to make another Editor function observe the edit.

### 3. Disk is fallback storage, not a competing live source of truth

If no working copy exists for a document, an Editor consumer may load the canonical saved document from disk and register/snapshot it as appropriate.

If a working copy exists, consumers MUST NOT silently reload disk simply because it is convenient. Doing so can resurrect stale data and create contradictory diagnostics.

Packaged players, command-line/headless processes, and a fresh Editor startup naturally begin from saved project files. This ADR changes coherence inside one live Editor process; it does not make unsaved memory globally persistent.

### 4. Save preserves the explicit persistence model

GameEngine does not autosave canonical authoring files after every operation. `Save`, `Ctrl+S`, and `Save All` persist the current canonically writable working copy through the existing validated atomic serialization path, then advance its saved baseline/clean state.

`Save All` must discover dirty documents across registered working-copy owners, including specialized asset editors. A panel-specific Save affordance may remain for ergonomics, but it is not required to publish edits inside the Editor.

Disk persistence remains separate from edit acceptance, in-process visibility, undo/redo, validation, preview/runtime snapshot construction, and crash recovery.

### 5. Undo and redo update the same working copy

Undo/redo is not private panel history from the perspective of readers. When an owning edit session restores a previous/next document state, the authoritative working-copy revision changes and consumers observe that state. Existing domain-specific undo implementations may remain.

### 6. Temporarily invalid working copies remain the current authoring state

Interactive editing may pass through an invalid intermediate state where the domain permits it. A consumer MUST NOT hide that state by falling back to the last saved valid file.

Instead:

- validators report the current problem;
- Scene View follows ADR 0068/0137 best-effort/presentation policy where applicable;
- strict operations that require validity may refuse to start or commit;
- previews may retain a documented last-good runtime result only when their preview contract explicitly allows it; and
- the UI distinguishes current-working-copy invalidity from saved-copy state.

### 7. Editor-initiated Play and preview snapshot from working copies

When an Editor operation creates a runtime/preview world from authored project state, every document for which a working copy exists is resolved from that working copy at the operation's defined snapshot boundary.

Strict Play may reject an invalid current working copy. It MUST NOT silently run the older saved Animation Set/Graph while the Editor displays a newer unsaved one.

After Play starts, runtime domains may or may not hot-apply later edits according to existing domain policy. ADR 0138 Graph Debug compares the running source generation with the latest working-copy revision and presents staleness explicitly.

### 8. Authoring working copy and preview asset residency are separate layers

ADR 0136 owns immutable conversion-ready CPU asset generations, GPU residency, preview streaming, and cache lifetime. This ADR owns mutable authoring documents. A working-copy edit may invalidate or advance derived preview asset generations through the appropriate domain/import path, but the working-copy registry MUST NOT become a mesh/texture/model cache and preview residency MUST NOT become the mutable authoring source of truth.

Animation Preview therefore reads current Graph/Animation Set authoring from the working-copy authority while obtaining renderable asset data through ADR 0136 residency.

### 9. External file changes use explicit conflict handling

A disk watcher or external tool may change a saved file while the Editor is open.

- If no working copy exists, a future load reads the new saved file.
- If the registered working copy is clean, the Editor may reload through the normal document-load path.
- If the working copy is dirty, the Editor MUST NOT silently overwrite it or silently merge disk bytes. It reports a conflict and requires a defined reload, compare, merge, or keep-working-copy decision.

Conflict handling uses document identity/revision and canonical parsed state, not timestamp-only assumptions.

### 10. Crash recovery is separate from canonical autosave

Crash recovery may journal or snapshot dirty working copies into user/private recovery storage. Recovery artifacts are not canonical project files and do not turn every edit into a Git-visible save. On restart, the Editor may offer recovery/reconciliation against the saved baseline.

### 11. Document identity is semantic and revisions are process-local

Working-copy lookup uses the existing stable document/asset/graph identity appropriate to the domain, plus canonical project-relative location where required by the document system. Process-local working-copy revision/generation values are observation and stale-write guards; they are not new project Stable IDs and are not serialized merely to support the registry.

Renames/moves must update registration through existing asset/document identity rules so one logical document does not remain registered under old and new paths.

## Scope / Non-goals

This ADR defines in-process document visibility and persistence semantics. It does not autosave canonical files after each edit, replace domain authoring commands/undo stacks, require one monolithic editor class, define multi-process collaborative authoring, create a second AI writer, change project serialization formats, require every Play domain to hot-reload every edit, or implement the registry in this ADR-only change.

## UX model

```text
edit
  -> authoritative Editor working copy changes immediately
  -> every Editor consumer sees that revision
  -> document becomes dirty
  -> undo/redo changes the same working copy
  -> Save / Save All persists to disk
  -> document becomes clean
```

Users reason about dirty/saved state, not which subsystem last reread disk.

## Diagnostic ownership

Working-copy validity diagnostics follow ADR 0137. A stale saved copy is never used simply to suppress current diagnostics. Save/persistence errors identify the document and persistence operation; domain validation errors remain domain-owned.

## Graph Debug architecture

Graph Debug uses this ADR as its source-resolution dependency. ADR 0138 owns shell/provider/runtime semantics. Here, `current source` means the authoritative working copy while `runtime source` is separately identified by the running instance generation.

## Domain-specific debug provider contract

Providers do not own document loading policy. They request source documents through the shared working-copy resolver and compare them with provider-owned runtime source identity. A provider MUST NOT bypass this contract by opening the asset path directly.

## Animation Preview responsibility boundary

Animation Preview reads the same current Animation Graph/Animation Set working copies as the rest of the Editor. Preview-specific playback controls remain transient. Preview render/resource residency follows ADR 0136. Saving is not required to refresh authoring preview.

## Working-copy / saved-copy model

```text
canonical saved file
       | load
       v
authoritative Editor working copy <-> domain edit session / undo
       |
       +--> Inspector / validation / Problems / Scene View / Preview
       |
       +--> Editor Play snapshot / Graph Debug source navigation
       |
       `--> explicit Save / Save All --> canonical saved file
```

No consumer-specific hidden disk copy participates while a working copy exists.

## AI Studio presentation boundary

AI Studio receives no bypass. Structured project mutation still uses the authoritative Editor writer from ADR 0121/0131. A detached or future external AI Studio frontend observes/mutates through Agent Host and existing authoring boundaries; it does not maintain an independently writable project working copy. ADR 0135 inference resource arbitration may suspend/reclaim presentation resources but MUST preserve dirty working-copy state.

## Stable ID / serialization impact

No canonical serialized schema changes are introduced. Existing Entity, Asset, Graph, Node, Motion Slot, material/document identities remain unchanged. Registry revisions, dirty bits, subscriptions, and recovery metadata are application state. Crash-recovery storage, if implemented, is explicitly non-canonical and versioned independently as needed.

## Public API / crate boundary impact

A shared Editor/application-layer working-copy read service or registry is expected. It may use authoring document identity/snapshot types but MUST NOT move Editor lifecycle into `engine-authoring`. Domain authoring services remain GUI-free. Runtime crates do not read the Editor registry directly; Editor composition resolves current authoring snapshots and passes/builds runtime inputs through existing boundaries.

Specialized editors expose current documents through the shared application contract instead of making consumers import panel implementations or reread their files. ADR 0136 preview residency remains a separate resource service.

## Migration / compatibility

Saved project files and schemas do not migrate. Existing `DocumentWorkspace` sessions are a compatible implementation precedent. Specialized editors can adopt the shared registry incrementally as long as, once registered, there is only one mutable working copy per document identity. Explicit Save remains; the UX compatibility change is that accepted unsaved edits become visible to other Editor subsystems.

## Testing strategy

Implementation must cover at least:

- an unsaved Animation Set edit is seen by validation and Scene View;
- an unsaved Animation Graph edit is seen by Inspector/preview/source navigation;
- disk remains unchanged until Save;
- Save All includes dirty specialized-editor documents;
- undo/redo updates consumer-visible snapshots;
- temporarily invalid working copies are diagnosed rather than replaced by saved state;
- strict Play rejects invalid current authoring rather than running stale disk;
- Play snapshots unsaved valid Graph/Set changes at startup;
- Graph Debug detects runtime-vs-working-copy staleness after later edits;
- dirty working copy + external disk change produces conflict;
- clean external refresh does not create duplicate owners; and
- move/rename does not leave two registrations for one stable document.

## Visual Validation requirements

This ADR-only documentation change requires no Visual Validation. Registry/coherency implementation is largely logic. Visual Validation is required when implementation changes dirty indicators, Save/Save All UI, conflict dialogs, preview stale-state presentation, or other visible document-state affordances.

## Rollout / implementation phases

1. Define document identity and read-only working-copy registry contracts in the Editor application layer.
2. Register existing `DocumentWorkspace` sessions without changing editing semantics.
3. Integrate Animation Set and Animation Graph first because cross-document binding exposes the current bug class.
4. Integrate Material and other specialized editors with separate in-memory documents.
5. Route Scene View, validation, preview, Problems, and Graph Debug source reads through the registry.
6. Route Editor Play snapshot construction through the same resolver.
7. Add external-change conflict UX and, separately, crash recovery if desired.

## Rejected alternatives

### Autosave every edit to canonical project files

Rejected. It creates Git churn, makes intermediate invalid state durable, and couples interactive latency to persistence.

### Require each specialized editor Save before other panels update

Rejected. Save is persistence, not publication inside one Editor process.

### Let each subsystem reread disk whenever it needs a document

Rejected. It creates contradictory views and incidental save-order behavior.

### Force every document type into one monolithic Editor class

Rejected. Domain-specific sessions/undo are legitimate; the shared contract is authoritative identity/read visibility.

### Fall back to the last saved valid copy when the working copy is invalid

Rejected. It hides the actual state being edited and can produce false preview/debug confidence.

### Make detached AI Studio or another frontend own a second working copy

Rejected. It violates the single authoritative Editor writer and stale-revision contracts.
