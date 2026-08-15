# Layered navigation and static triangle queries

## Advanced Geometry Designer GUI

Open a project in Engine Editor, select **Authoring Tools**, then open **Advanced
Geometry Designer**. The designer appears as a modeless window inside the current
editor process; no standalone designer executable or Cargo command is required.

The NavMesh Layers tab creates named height layers, edits bake bounds and agent
settings, adds AABB obstacles, and displays the baked grid dimensions and
walkable-cell count.

The Floor Links tab creates directional or bidirectional stairs, lifts, and drop
links using layer pickers and world positions. Its path-query test accepts a
start and destination and displays every generated waypoint.

The Static Mesh tab edits indexed vertices and triangles, validates bounds and
degenerate geometry, and provides a finite raycast test showing the hit triangle,
position, distance, and normal.

New, Open, Save, and Save As persist an `advanced-geometry.json` authoring file.
Saving is blocked until every layer, link, and static mesh passes the same runtime
constructors used by the path and raycast previews.

## Runtime queries

`engine::LayeredNavMesh` composes existing grid NavMeshes into named height
layers and connects floors with explicit directional links. Current path queries
support same-layer routes, routes crossing one authored link, bidirectional
links, and selection of the shortest reachable direct link.

`engine::StaticTriangleMesh` validates immutable indexed triangle geometry,
computes its bounds, and provides closest-hit finite raycasts for selection,
ground probing, and future baked static mesh-collider integration. It does not
silently enter the fixed-step collision pipeline.
