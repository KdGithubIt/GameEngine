# Renderer Limits — Editor Ready v1

These deterministic authoring ceilings are validated before Play and package
analysis. They are intentionally no larger than the supported runtime path;
device-specific capability queries may impose a lower quality setting but do
not silently expand scene semantics.

| Resource | Limit | Diagnostic behavior |
|---|---:|---|
| Texture width or height | 8,192 px | Blocking asset diagnostic |
| Material texture slots | 3 (base, normal, emissive) | Fixed schema; additional slots are unsupported |
| Joints per skin | 128 | Import diagnostic; oversized skin is not instantiated |
| Bones per skeleton asset | unbounded | The 128 cap applies to one skin binding, which is what a draw uploads (ADR 0086 §4) |
| Directional lights | 1 | Warning; first stable authoring entity wins |
| Ambient lights | 1 | Warning; first stable authoring entity wins |
| Particles per emitter | 65,536 | Blocking component diagnostic |
| Worst-case render instances per scene | 100,000 | Blocking scene diagnostic |
| Particle spawns per emitter per frame | 256 | Runtime burst clamp |

The worst-case instance budget is the sum of authored mesh/skinned/LOD
entities and every emitter's `max_particles`. It is deliberately conservative:
content must be safe even when all pools are full in the same frame.
