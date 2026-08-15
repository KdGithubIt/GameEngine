# ADR 0117: Project-First Launcher and Editor Application Lifecycle

Status: Accepted
Date: 2026-08-15
Amends: ADR 0023
Related: ADR 0031, ADR 0115

## Context

The current editor owns both project selection and project editing. It can start
without a `ProjectRoot`, restores a last project after the GUI has already been
constructed, shows the Project Hub from a no-document state, and switches
projects by rebinding a large set of project-scoped editor subsystems in place.
Phase 26 intentionally added that Hub as UX over ADR 0023, but the resulting
application lifecycle now conflates four distinct responsibilities:

1. discovering or selecting a project;
2. creating a complete GameEngine project;
3. acquiring exclusive ownership of a project for editing; and
4. running an editor workspace whose assets, documents, watchers, imports,
   build state, and game module all belong to that project.

That conflation weakens invariants. A normal editor workspace must repeatedly
account for the absence of a project, project switching requires every
project-scoped cache and service to be reset correctly, recent-project state is
mixed with editor workspace state, and a second editor process can bypass a
launcher-only duplicate-open check.

ADR 0023 remains correct that `ProjectRoot`, `ProjectConfig`, `project.json`,
and project path confinement are GUI-free authoring responsibilities. The new
problem is above that boundary: process lifetime, project acquisition, project
creation policy, exclusive editor ownership, and launcher/editor coordination.
Those concerns need a shared application-lifecycle boundary without moving
canonical authoring semantics into GUI code.

## Decision

### 1. Use a project-first two-application model

GameEngine has two distinct desktop application roles:

- **GameEngine Launcher / Project Manager** selects existing projects, creates
  new projects, owns recent-project UI state, reports project/editor
  compatibility, and starts or activates editor processes.
- **GameEngine Editor** edits exactly one project for its process lifetime.

The Launcher and Editor are separate executables. The Launcher remains running
while editors that it started are running and is single-instance per user
session. Multiple Editor processes MAY run concurrently when they edit distinct
project locations.

The normal Editor launch contract is an explicit project path, conceptually:

```text
engine-editor --project <path>
```

The Editor MUST NOT infer its active project from recent-project preferences.
Starting the Editor without a project is not a normal editing state. Recovery
or safe-start options MAY suppress optional project subsystems, but they MUST
NOT create a project-less `EditorWorkspace`.

The Editor performs the authoritative `ProjectRoot::open` validation at process
startup even when the Launcher already inspected the path. Once that succeeds,
the editing workspace is constructed with a concrete `ProjectRoot`; a normal
workspace does not store the active project as `Option<ProjectRoot>`.

`ProjectRoot::open` success means that the project identity/root contract is
valid. It does not mean every editor subsystem is healthy. Failures in game
module loading, assets, imports, shaders, or other recoverable project content
are editor diagnostics and do not retroactively make the project root invalid.

### 2. Add a shared project application-lifecycle crate

A dedicated GUI-free shared crate, `engine-project-lifecycle`, owns application
lifecycle around an authoring project. It sits above `engine-authoring` and MAY
depend on `engine-authoring`, but it MUST NOT own authoring commands, runtime
ECS semantics, editor GUI state, or Launcher UI.

Its responsibilities are:

- acquiring and validating a project for application use through
  `ProjectRoot`;
- coordinating the standard GameEngine new-project scaffold;
- acquiring and holding the exclusive editor lease described below;
- recording ephemeral ownership metadata for diagnostics;
- exposing process-neutral lifecycle status/contracts needed by Launcher and
  Editor; and
- validating project/editor compatibility metadata before an editor workspace
  is started.

The Launcher and Editor are clients of this crate. CLI tooling MAY reuse it when
it needs the same application-level project lifecycle, but ordinary authoring
commands continue to use `engine-authoring` directly.

`engine-authoring` remains the owner of `ProjectRoot`, `ProjectConfig`,
`project.json`, path confinement, persisted authoring data, commands,
transactions, and validation. The lifecycle crate does not become a generic
project or authoring utility crate.

### 3. Replace in-process project rebinding with process replacement

An Editor process never changes from Project A to Project B by resetting and
rebinding project-scoped state.

A project switch follows this lifecycle:

1. the current Editor resolves its dirty-document guard;
2. the Launcher is activated (or started if the Editor was launched directly);
3. cancel leaves the current Editor unchanged;
4. after Project B is selected, a new Editor for B is started or an already
   running Editor for B is activated;
5. the current Editor stays alive until the B editor has acquired its lease,
   opened its `ProjectRoot`, and reported minimum bootstrap readiness; and
6. only then is the old Editor asked to close.

Failure to select, validate, lease, spawn, or bootstrap Project B leaves the
Project A editor running. Full asset import or complete subsystem health is not
part of minimum bootstrap readiness.

Launcher/Editor IPC is limited to application lifecycle coordination such as
single-instance activation, switch requests, editor activation, readiness, and
close-after-switch. It MUST NOT carry authoring mutations or become a second
project-data API. The concrete IPC transport is an implementation detail.

### 4. Enforce one Editor per canonical project location

At most one Editor process may hold editing ownership of the same canonical
project root at a time. This invariant applies even when `engine-editor` is
started directly rather than through the Launcher.

The lifecycle crate enforces ownership with two complementary mechanisms:

- an **OS-backed exclusive lease** is authoritative for exclusion and is
  released automatically when the owning process exits or crashes; and
- **ephemeral ownership metadata** records diagnostic information such as the
  owning process and start time.

Metadata is not authoritative. If a previous process crashes and leaves stale
metadata, availability of the OS lease proves that the old ownership is gone.
Ownership metadata MUST live outside canonical project authoring data and MUST
NOT be stored in `project.json`, `project_settings.json`, the asset manifest, or
other packaged/project-versioned documents.

The lease key is based on the canonical project location, not only on the
stable project ID. Two independent working copies of the same logical project
therefore do not incorrectly lock each other.

When the Launcher is asked to open a project location whose Editor already owns
the lease, it activates that Editor instead of starting a duplicate process.

### 5. Separate project identity from project location

`project.json` remains the minimal project identity document owned by
`engine-authoring`, but its next schema revision MUST add a stable `ProjectId`
and explicit engine-version association. `ProjectId` uses the existing stable
identifier convention with the `project_` prefix and MUST survive directory
moves and renames.

The engine association identifies the GameEngine distribution/version for
which the project identity contract is current. The Launcher uses it together
with the project schema to choose or reject an Editor before launch, and the
Editor validates compatibility again before constructing its workspace. Until
a broader compatibility policy is explicitly defined, an Editor accepts only
the current project schema and a compatible current engine association.

A project location is the pair of logical identity and canonical root. Copies
or independent working trees can therefore share a logical `ProjectId` while
remaining distinct locations for locking and process ownership. User-state
storage MUST distinguish simultaneously present locations when necessary and
MUST NOT assume that `ProjectId` alone proves filesystem identity.

### 6. Separate low-level project creation from the standard scaffold

`ProjectRoot::create` remains the low-level operation that establishes the
canonical project root contract. It does not absorb Launcher UI or template
policy.

The lifecycle layer coordinates the higher-level **standard GameEngine project
scaffold**. The standard scaffold is the single reusable creation path for the
Launcher and other application-level creators and includes the product policy
needed for a newly created project to be editor-ready, such as project
identity, standard directories, game-project initialization, project settings,
and starter/template authoring content.

Creation is transactional from the user's point of view. The lifecycle creates
and validates the complete scaffold in a staging location and publishes the
final project location only after all required creation steps succeed. A failed
creation MUST NOT be presented as a valid new project and SHOULD clean up its
staging data while preserving actionable diagnostics.

Starter-scene or template policy MUST NOT be duplicated in Launcher and Editor
GUI code.

### 7. Split application preferences from project-scoped editor workspace state

Recent projects and the last selected project are Launcher/application user
state. They are not authoring data and no longer belong to Editor workspace
preferences. This amends ADR 0023 decision 4 only with respect to which UI
application owns those preferences; they still MUST NOT enter
`engine-authoring` or canonical project files.

Editor workspace state such as open documents, panel/layout state, selected
asset folders, and other presentation/session restoration data is stored as
per-project user data outside the project working tree. `ProjectId` is its
logical identity anchor, while storage must also handle multiple simultaneous
locations for the same ID without write collisions.

No recent-project, window, selection, or workspace-restoration state is added
to `project.json` or `project_settings.json`.

### 8. Keep compatibility failure explicit and current-format-only

This ADR does not restore compatibility readers or automatic migrations removed
by ADR 0115. The Launcher owns the user-facing detection and explanation of an
incompatible project, but normal project open does not silently rewrite it.

When the `project.json` identity revision required by this ADR is implemented,
its schema version is bumped and all repository-owned current projects,
fixtures, and writers are updated in the same change. Once that new schema is
the current format, the previous schema is rejected in accordance with ADR
0115 rather than accepted through a fallback reader.

A future explicit conversion facility, if desired, requires its own documented
contract and must not turn Editor startup into an implicit irreversible
migration path.

## Consequences

- The strongest normal editor invariant becomes `EditorWorkspace =>
  ProjectRoot`; project absence is removed from ordinary editing state.
- Project selection and no-document editing state are no longer conflated. A
  project with no open document remains a valid project-scoped Editor
  workspace, not a Project Hub.
- Project switching becomes process lifecycle rather than a growing list of
  per-subsystem reset operations, reducing cross-project state leakage.
- Launcher and Editor gain a stable process boundary that can later support
  installed Editor-version selection without moving project-selection logic out
  of the Editor a second time.
- A new workspace crate and two-application lifecycle add implementation and
  IPC complexity, but that complexity is isolated from authoring semantics and
  runtime ECS code.
- Exclusive editor leases prevent silent same-working-tree write races across
  scenes, settings, imports, generated files, build outputs, and other
  project-scoped state.
- Atomic scaffolding prevents half-created directories from being reported as
  valid new projects.
- Moving a project does not erase its logical identity, while canonical-path
  location remains available for locking and distinguishing multiple working
  copies.
- Phase 26's Project Hub remains useful historical context, but its placement
  inside the Editor and its ownership of recent-project state are superseded by
  this ADR.

## Alternatives Considered

### Keep Project Hub and project switching inside the Editor

Rejected. It preserves the weak project-optional workspace state and requires
all project-scoped editor services to participate correctly in every in-process
project reset.

### Use a Launcher but keep the Editor project-optional

Rejected. A separate Launcher would improve UX without establishing the
lifecycle invariant that justifies the process boundary.

### Rebind one Editor process from one project to another

Rejected. The existing reset/rebind approach scales poorly as project-scoped
watchers, imports, caches, game modules, build state, and documents grow.
Process replacement provides a clear ownership lifetime.

### Put lifecycle ownership in `engine-authoring`

Rejected. Project leases, process ownership, Launcher coordination, and
application scaffolding are not authoring data-model responsibilities. ADR 0023
continues to own `ProjectRoot` in `engine-authoring`.

### Put shared lifecycle rules in the Launcher

Rejected. Direct Editor launch and non-Launcher clients must enforce the same
lease and project lifecycle rules. Depending on Launcher code would invert the
shared dependency direction.

### Enforce duplicate-open prevention only in the Launcher

Rejected. Direct `engine-editor --project` launch would bypass the invariant.

### Use only a lock file or only ownership metadata

Rejected. A persistent file alone requires unreliable stale-lock heuristics,
while an OS lease alone provides poor diagnostics. The OS lease is authoritative
and metadata is descriptive.

### Leave project identity path-only

Rejected. Directory moves would lose logical per-project user identity and make
future Launcher/editor-version management depend on installation-specific
paths.

### Keep New Project creation in Editor GUI code

Rejected. A separate Launcher would duplicate product scaffolding policy and
other clients could create structurally different projects.

## Compatibility and Migration

This ADR is an architectural documentation change and does not itself modify
on-disk project files or public Rust APIs.

Implementation requires a new `engine-project-lifecycle` workspace crate, a
Launcher executable, an explicit Editor project-launch contract, and a
`project.json` schema revision for `ProjectId` and engine association. Those are
deliberate contract changes authorized by this ADR and MUST be implemented with
the repository's normal versioned-schema and validation rules.

ADR 0023 remains authoritative for `ProjectRoot`, `ProjectConfig`,
`project.json`, and path-safety ownership in `engine-authoring`; decision 4 is
amended so recent-project history belongs to the Launcher/application rather
than the Editor. ADR 0031 remains authoritative for `project_settings.json` and
its separation from project identity. ADR 0115 remains authoritative for the
current-format-only baseline: the implementation updates the current schema and
repository-owned data together and does not add a legacy read/migration path.
