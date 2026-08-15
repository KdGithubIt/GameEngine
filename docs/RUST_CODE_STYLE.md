# Rust Code Style and Documentation Standard

Status: Accepted  
Version: 1.0.0  
Canonical location: `GameEngine/docs/RUST_CODE_STYLE.md`

## 1. Purpose

This document defines the required Rust coding, documentation, error handling,
safety, testing, and dependency standards for GameEngine.

It applies to human contributors, Codex, Claude, and other tools that create or
modify Rust code in this project.

## 2. Language

All text inside source code MUST be written in English.

This includes:

- Identifiers
- Module, type, function, and variable names
- Rustdoc comments using `//!` and `///`
- Implementation comments using `//`
- `TODO`, `FIXME`, `SAFETY`, and similar comments
- Developer-facing log messages
- Error codes and developer-facing error messages
- Test names

Japanese MAY be used in design discussions and documents intended for Japanese
readers, but it MUST NOT be added to Rust source files.

Reasons:

- Rust terminology stays consistent with the standard library, compiler, and
  ecosystem.
- Human and AI contributors can search for the same terms used by APIs and
  diagnostics.
- Documentation cannot become inconsistent because one of two translations was
  not updated.
- Generated API documentation remains concise.

## 3. Formatting and Linting

All Rust code MUST be formatted with the project's stable Rust toolchain.

Required checks:

```text
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo doc --workspace --no-deps
```

Contributors MUST NOT introduce new compiler, Clippy, test, or rustdoc warnings.
The long-term repository target is zero warnings for all required checks.

`#[allow(...)]` MUST be narrowly scoped and MUST include an adjacent English
comment explaining why the lint does not apply.

```rust
// This type is retained as a serialized compatibility marker.
#[allow(dead_code)]
struct LegacyMarker;
```

Formatting preferences that differ from `rustfmt` MUST NOT be enforced manually.

## 4. Naming

Code MUST follow the Rust API Guidelines and standard Rust naming conventions.

- Types and traits use `UpperCamelCase`.
- Functions, methods, modules, variables, and fields use `snake_case`.
- Constants use `SCREAMING_SNAKE_CASE`.
- Stable identifier types use explicit names such as `EntityId`, `GraphId`,
  `NodeId`, and `AssetId`.
- Boolean names SHOULD read as predicates, such as `is_visible`, `has_parent`,
  or `can_connect`.
- Names such as `data`, `value`, `manager`, `helper`, and `util` SHOULD be
  avoided when a more specific name exists.

Getter methods SHOULD omit the `get_` prefix:

```rust
fn health(&self) -> &Health;
fn health_mut(&mut self) -> &mut Health;
```

## 5. Public API Documentation

Every public module, type, trait, function, method, field, enum variant, and
associated constant MUST have English rustdoc unless it is intentionally
excluded from the public API.

- Use `//!` for crate and module documentation.
- Use `///` for public items.
- Explain the contract, not merely the item name.
- Document invariants, side effects, ownership, failure conditions, and ID
  lifetime when relevant.
- Link related public items with rustdoc links where useful.
- Include examples for non-obvious public APIs.
- Examples SHOULD compile as doctests when practical.

```rust
/// Creates a runtime entity without components.
///
/// The returned [`Entity`] is valid until it is despawned from this world.
///
/// # Errors
///
/// Returns an error if the allocator cannot reserve another entity ID.
pub fn spawn(&mut self) -> Result<Entity, SpawnError> {
    // ...
}
```

Rustdoc SHOULD use these standard sections when applicable:

- `# Examples`
- `# Errors`
- `# Panics`
- `# Safety`

Private items SHOULD be documented when their purpose, invariants, or behavior
are not clear from their name and type signature.

Reasons:

- Public APIs are contracts between crates and future contributors.
- `cargo doc` can generate a usable API reference automatically.
- AI tools can understand correct usage without reverse-engineering the
  implementation.
- APIs that are difficult to document are often too complex or ambiguous.

## 6. Implementation Comments

Comments MUST explain why code exists, which invariant it preserves, or why an
obvious-looking alternative is incorrect.

Comments MUST NOT narrate code that is already clear from the implementation.

```rust
// Preserve child order because Behavior Tree execution order is semantic.
children.push(child);
```

Commented-out code MUST NOT be committed. Use version control instead.

`TODO` and `FIXME` comments MUST describe the unresolved issue and SHOULD link
to an issue, ADR, or specification section when one exists.

```rust
// TODO(authoring-format): Preserve unknown fields after ADR 0001 is accepted.
```

Stale or incorrect comments are defects and MUST be updated or removed when
related code changes.

## 7. Error Handling and Panics

Library code MUST NOT use `unwrap()`, `panic!()`, or unchecked indexing for
recoverable failures.

Library code SHOULD return typed errors or structured diagnostics for invalid
input, missing data, failed I/O, invalid authoring state, and other expected
failures.

```rust
let graph = graphs
    .get(&graph_id)
    .ok_or_else(|| GraphError::NotFound(graph_id.clone()))?;
```

`expect()` MAY be used only when an invariant has already been established and
the message explains that invariant.

```rust
let root = roots
    .next()
    .expect("validated behavior tree must contain exactly one root");
```

Tests and examples MAY use `unwrap()` or `expect()` when failure should abort
the test or example and the usage remains readable.

Public functions that may panic MUST document the condition in a `# Panics`
rustdoc section.

Reasons:

- Engines and editors must handle invalid assets and partially edited data
  without crashing.
- CLI and MCP adapters need actionable failure information.
- Transactions need errors that can be reported before rollback.

## 8. Unsafe Code

Unsafe code MUST be minimized and isolated behind a safe API whenever possible.

Every `unsafe` block MUST have an immediately preceding `// SAFETY:` comment
that explains the exact conditions that make the operation sound.

```rust
// SAFETY: The scheduler guarantees exclusive access to this component storage
// for the duration of the query.
unsafe {
    storage.get_unchecked_mut(index)
}
```

Unsafe functions MUST include a `# Safety` rustdoc section describing caller
obligations.

Safety invariants SHOULD be enforced by types and MUST be covered by focused
tests where practical.

An unsafe block without a clear safety explanation MUST NOT be accepted.

Reasons:

- The compiler cannot verify the assumptions inside unsafe code.
- ECS implementations are especially sensitive to aliasing and lifetime
  mistakes.
- Future contributors and AI tools need to know which conditions must remain
  true when surrounding code changes.

## 9. Visibility and API Design

Visibility MUST be as narrow as practical.

- Prefer private items by default.
- Use `pub(crate)` or `pub(super)` when external crates do not need access.
- Add `pub` only for a deliberate external contract.
- Do not expose implementation details solely to make tests easier.
- Prefer types that make invalid states difficult to represent.
- Prefer explicit domain types over ambiguous primitive values.

New abstractions SHOULD solve a demonstrated problem or match an established
project pattern. Do not create generic frameworks for a single use case without
a clear near-term need.

## 10. Crate and Module Dependencies

Dependencies MUST follow the ownership boundaries in
`AI_FRIENDLY_AUTHORING_SPEC.md`.

- Runtime ECS MUST NOT depend on authoring, GUI, CLI, MCP, or graph view types.
- Authoring core MUST NOT depend on a GUI framework or MCP transport.
- CLI, MCP, and editor code MUST be thin adapters over shared core logic.
- Graph domains MUST reuse the domain-neutral graph model.
- Circular crate dependencies MUST NOT be introduced.

New third-party dependencies MUST have a clear purpose and SHOULD be evaluated
for maintenance status, license, feature size, and impact on compile time.

Cargo features SHOULD represent meaningful optional capabilities, not minor
implementation details.

## 11. Testing

Tests are required for:

- Bug fixes
- Authoring commands
- Transactions, undo, and redo
- Serialization and migrations
- Stable identifiers
- Graph validation
- Diagnostic codes
- Unsafe code and its safety invariants
- Authoring-to-runtime conversion

Test names MUST describe expected behavior.

```rust
#[test]
fn pinned_nodes_do_not_move_during_auto_layout() {
    // ...
}
```

Tests SHOULD:

- Assert behavior rather than internal implementation details.
- Use stable diagnostic codes instead of matching full error messages.
- Avoid nondeterministic timing and ordering assumptions.
- Keep golden files small and reviewable.
- Add a regression test before or with a bug fix.

## 12. Generated Documentation

The project uses rustdoc as its API documentation system.

Generate documentation with:

```text
cargo doc --workspace --no-deps
```

Generated documentation MUST NOT be committed unless a future ADR explicitly
requires it.

Broken rustdoc links, invalid doctests, and rustdoc warnings are treated as
code quality defects.

## 13. Existing Code and Migration

This standard applies immediately to all new and modified code.

Existing code may temporarily contain warnings, missing rustdoc, non-English
comments, or patterns that do not comply with this standard. Contributors MUST
NOT use existing violations as precedent.

When modifying a file, contributors SHOULD improve nearby violations when the
change is small and does not create unrelated behavioral risk. Large cleanup
work SHOULD be performed as a separate, reviewable change.

## 14. Review Checklist

Before considering a Rust change complete, verify:

- Source code text is English.
- `cargo fmt --all --check` passes.
- No new compiler, Clippy, rustdoc, or test warnings were introduced.
- Public API changes include rustdoc.
- Recoverable failures return errors or diagnostics.
- Every unsafe block has a valid `// SAFETY:` comment.
- Required tests were added or updated.
- Crate ownership boundaries remain intact.
- Persisted formats and stable IDs were not changed silently.
