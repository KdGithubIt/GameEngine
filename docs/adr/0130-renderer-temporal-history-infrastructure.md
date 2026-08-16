# ADR 0130: Renderer Temporal History Infrastructure

Status: Accepted
Date: 2026-08-16
Relates to: ADR 0003, ADR 0040, ADR 0107, ADR 0118, ADR 0119

## Context

The renderer now has stable linear-HDR scene color, shared static/skinned
material shading, full image-based lighting, generic local lights, stable CSM,
transparency coverage, and Generic ToonLit quality. The next rendering phases
need frame-to-frame state for temporal anti-aliasing and other temporal effects,
but the current `WorldRenderer` writes each resolved scene frame directly into
a caller-owned single-sample color view and retains no previous scene color or
camera state.

Adding TAA immediately would mix several independent contracts at once:
sub-pixel camera jitter, history lifetime, camera-cut invalidation, motion
vectors or reprojection inputs, and a temporal resolve filter. In particular,
skinned motion vectors need previous-pose data rather than a static-only
shortcut. The renderer first needs a stable history boundary that later effects
can consume without changing authoring data or caller texture usage.

## Decision

### 1. `WorldRenderer` owns process-local scene-color history

The public render facade owns two single-sample scene-color textures in the
same format and dimensions as the renderer output. The ordinary backend renders
into the current history texture instead of directly into the caller-owned
color view. After the frame, a fullscreen exact-copy pass writes the completed
scene color into the caller view, then the history pair advances.

Both history textures use `RENDER_ATTACHMENT | TEXTURE_BINDING`. The caller's
existing color-view contract remains unchanged: temporal infrastructure does
not require callers to add `COPY_SRC`, `COPY_DST`, or a new bindable usage to
their textures.

The previous history texture is retained even though this phase does not yet
sample it. This establishes the resource lifetime and ping-pong ownership that
a later temporal resolve will use.

### 2. History invalidation is explicit and deterministic

History is invalid after allocation or resize and becomes available only after
a successful frame completes. Changing the selected active camera entity also
invalidates accumulation for that frame. Camera identity follows the existing
ADR 0107 selection result, including runtime entity generation, so selection
order is not reimplemented.

`WorldRenderer::reset_temporal_history()` is the explicit camera-cut API for a
discontinuous transform or projection change that intentionally keeps the same
camera source. The renderer does not guess cuts from translation, rotation, or
matrix thresholds because those heuristics would impose undocumented gameplay
semantics.

### 3. Temporal camera metadata is renderer-owned and unjittered in this phase

For every prepared frame the renderer records:

- the current unjittered view-projection matrix;
- the previous usable unjittered view-projection matrix;
- the current and previous low-discrepancy sub-pixel jitter candidates; and
- whether history is valid for accumulation.

Jitter uses a deterministic eight-sample Halton sequence in bases 2 and 3 and
is normalized to NDC from the current render-target extent. The sequence is
prepared now so future temporal resolve work does not invent a second frame
index or sampling convention.

This phase deliberately does **not** apply jitter to the camera projection.
Without a temporal resolve, applying it would make the presented frame visibly
shake. Current rasterization therefore remains bit-for-bit governed by the
existing unjittered camera matrices apart from the new internal output copy.

### 4. TAA, motion vectors, and depth history remain separate follow-up work

This ADR does not define a TAA blend filter, neighborhood clamp, reactive mask,
velocity buffer, depth-history format, or previous object/skin transform
contract. Static and skinned geometry continue to use the existing main-pass
vertex contracts.

When motion vectors are introduced, skinned meshes must receive a real previous
pose/palette contract rather than silently reporting only rigid transform
motion. Any new persisted quality setting or author-facing TAA control requires
a separate authoring/settings decision.

## Consequences

- Future temporal effects have stable current/previous scene-color ownership
  and deterministic frame metadata.
- Resize and camera-source switches cannot accidentally reuse incompatible
  history.
- Explicit cuts can invalidate history without embedding gameplay-specific
  motion thresholds in the renderer.
- Existing hosts keep their caller-owned HDR texture usage and tone-map flow.
- One fullscreen copy pass is added per rendered frame before tone mapping.
- This phase does not claim TAA or motion-vector support; presented projection
  remains unjittered.

## Alternatives Considered

### Require caller HDR textures to support copy operations

Rejected because `WorldRenderer` currently accepts caller-owned render targets
whose usage is outside the renderer's control. Requiring `COPY_SRC` or
`COPY_DST` would silently strengthen a public runtime contract merely to
implement an internal history mechanism.

### Store history in the windowed application host

Rejected because Editor previews and other renderer consumers also need the
same temporal lifetime rules. History belongs to the stateful renderer facade,
not one application shell.

### Implement TAA and skinned motion vectors in the same change

Rejected because it would couple history ownership to an incomplete motion
contract. Establishing the lifetime and invalidation boundary first keeps the
change reversible and lets later TAA work validate reprojection independently.

### Detect camera cuts from motion thresholds

Rejected because a threshold that is correct for one game or camera controller
is not a generic renderer invariant. Camera-source changes are deterministic;
other intentional cuts use the explicit reset API.

## Compatibility and Migration

No serialized scene, project setting, Material field, Stable ID, importer, or
asset format changes are introduced. `WorldRenderer::reset_temporal_history()`
is an additive runtime API. Existing render calls keep the same parameters and
caller-owned color/depth target requirements. Resize invalidation is automatic,
so existing hosts do not need a migration step.
