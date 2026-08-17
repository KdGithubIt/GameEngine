# ADR 0152: Multi-Agent Writer Coordination and Conflict Ownership

Status: Proposed
Date: 2026-08-17
Builds on: ADR 0131, ADR 0139, ADR 0151
Relates to: ADR 0005, ADR 0121, ADR 0141

## Context

ADR 0131 intentionally permits multiple read-oriented sessions but only one AI run per Editor project to hold the writer role. Human editing remains allowed because stale revision/generation checks provide a clear conflict boundary. A future workflow may benefit from several AI agents working on independent domains, but simply removing the single-writer guard would create ambiguous ownership of documents, source files, permissions, repair loops, and completion evidence.

## Decision

GameEngine introduces multi-agent writing only through explicit **work ownership** and **conflict detection** above the existing authoring/code-workspace boundaries. One project authority remains responsible for canonical mutations; multiple agents do not become independent file writers.

## Work claims

A write-capable run declares a bounded working set at an appropriate semantic level, such as:

- authoring document identities;
- source files/modules or code-workspace paths;
- asset acquisition targets; and
- shared resources that require exclusive coordination.

Claims are run-control/application metadata, not canonical project data. Claims may be refined as inspection discovers additional required scope. Expanding into a conflicting claim requires waiting, re-planning, or an explicit conflict resolution decision.

## Authoring conflicts

Authoring mutations still use ADR 0121/0139 authoritative revisions and transactions. A claim does not bypass stale-revision rejection. Two agents whose scopes are disjoint may progress concurrently; conflicting changes serialize at the authoritative service or cause one agent to re-inspect/re-plan.

## Source-code conflicts

Each AgentSession/Run retains governed code-workspace checkpoints/diffs. Multi-agent integration must detect overlapping source edits against a common authoritative baseline. Automatic merge is allowed only when it is structurally safe and reviewable. Conflicts become explicit run evidence; agents MUST NOT overwrite another run's applied change by force.

## Human editing

Human edits remain outside AI claim ownership and are always allowed. They may invalidate one or more agent assumptions and cause stale rejection/replanning. AI claims cannot lock the human out of the Editor.

## Completion and audit

Each run keeps its own proposal snapshot, permissions, events, validation evidence, and completion report. Project-level orchestration may summarize dependency relationships between runs, but one run cannot claim completion based solely on another run's unverified output.

Audit history records claim acquisition/release, waits/conflicts, cross-run dependencies, reconciliations, and final applied changes.

## Scheduling

The first multi-writer implementation SHOULD prefer conservative concurrency: allow obviously disjoint read/authoring/code scopes and serialize ambiguous/shared scopes. Optimistic broad parallelism is not a correctness goal.

## Dependencies and parallel work

This ADR is Wave D. It follows ADR 0151 writer ownership and should use a stable ADR 0141 native writer as one representative client. It MUST NOT be implemented by depending on unmerged sibling branches.

## Verification

Tests must cover disjoint concurrent work, overlapping document claims, overlapping source edits, human edits during both runs, stale-revision rejection, run cancellation releasing claims, crash/restart cleanup, explicit conflict reporting, and independent completion gates.

Multi-agent status/conflict UI requires Editor Visual Validation.

## Non-goals

This ADR does not provide real-time multi-user collaboration, distributed CRDT project files, automatic resolution of every Git/source conflict, or permission for several processes to bypass one authoritative project writer.
