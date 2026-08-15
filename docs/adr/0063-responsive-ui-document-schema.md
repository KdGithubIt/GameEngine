# ADR 0063: Responsive UI Document Schema

Status: Accepted

Date: 2026-07-19

## Context

UI schema version 2 stores fixed pixel values and individual panel anchors but
does not define a reference resolution, scale policy, safe area, or reusable
per-element constraints. Preview and Game View therefore cannot share a
deterministic responsive calculation.

## Decision

UI schema version 3 adds document-level reference resolution, scale policy,
safe-area padding, and a stable node-ID keyed constraint map. Scale policies
are constant pixels, viewport scaling by width, height, or width-height blend,
and constant physical size with a reference DPI. Constraints contain optional
minimum/maximum size, aspect ratio, and anchor minimum/maximum values.

Version 1 and 2 documents migrate in memory with a 1920x1080 reference,
constant-pixel policy, zero safe-area padding, and no constraints. These
defaults preserve prior appearance. Preview and runtime call the same pure
scale and safe-viewport calculation. Unsupported future versions remain
rejected.

## Consequences

Responsive intent is persisted and reviewable. Existing documents retain their
fixed-pixel behavior until explicitly changed. Node IDs remain the constraint
identity, so rename commands must migrate matching constraint keys.

