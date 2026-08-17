# ADR 0146: Governed AI Asset Acquisition and Generative Content Providers

Status: Proposed
Date: 2026-08-17
Builds on: ADR 0131
Relates to: ADR 0021, ADR 0029, ADR 0075, ADR 0121

## Context

ADR 0131 reserves an asset acquisition service for AI workflows. The current agent can reason about existing project assets but does not provide a governed service for searching external catalogs, downloading content, or requesting generated images, audio, or 3D assets. Falling back to arbitrary shell/curl writes would bypass manifest/import rules and make provenance and permissions provider-specific.

## Decision

GameEngine adds a GUI-free **Agent Asset Acquisition Service** above the normal asset import/manifest pipeline. AI runtimes request typed search/acquire/generate operations; providers perform remote/local acquisition; accepted content then enters existing GameEngine import and registration paths.

Provider classes may include:

- external asset catalogs;
- local asset libraries;
- image generation;
- audio generation;
- 3D/model generation; and
- future organization-managed content sources.

A provider does not receive direct authoring authority merely because it returns files.

## Permissions

Network acquisition requires ADR 0131 network permission. Writing acquired assets requires the appropriate asset-write capability. Paid generation, external licensing acceptance, unusually large downloads, or provider credential use may require an additional explicit user decision when the provider contract cannot be covered safely by the existing permission grant.

## Provenance and licensing

When available, acquired content records reviewable source/provider identity, original asset identifier/URL or generation request identity, license/provenance metadata, and acquisition timestamp/version. This metadata is collaboration/audit information and must not contain provider credentials or temporary download tokens.

Generated-content prompts and provider responses may be retained in AI audit/session history when allowed by session privacy policy, but they are not canonical gameplay data unless deliberately authored into a project document.

## Import boundary

Providers return bounded acquisition artifacts into a controlled staging area. Final project paths are chosen through GameEngine path confinement and normal asset import semantics. Providers MUST NOT write arbitrary project locations, mutate `asset_manifest.json` directly, or bypass importer validation.

Failed acquisition/import is structured evidence for repair or user action; a downloaded-but-unimportable artifact is not reported as a completed project asset.

## Remote AI Studio

Remote clients may approve a pending asset/network permission and view summarized provenance/status, but provider credentials, temporary download URLs, raw filesystem paths, and unrestricted provider output are sanitized under ADR 0133.

## Dependencies and parallel work

This ADR can be implemented in parallel with ADR 0141-0145 and ADR 0147-0149. The native Agent from ADR 0141 can consume the service when both are on `main`, but external Agent Runtimes may also use it through the same host capability.

## Verification

Implementation must cover network/asset permission enforcement, project-root confinement, provider failure mapping, import-pipeline reuse, provenance retention, no credential persistence, duplicate/idempotent acquisition where relevant, cancellation, and remote sanitization.

Asset-acquisition UI and provenance review require Editor Visual Validation.

## Non-goals

This ADR does not choose one commercial asset marketplace or generator, permit arbitrary web downloads by default, or define new canonical asset formats.
