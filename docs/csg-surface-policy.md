# CSG shading and subdivision

Applies to `union`, `difference`, `intersect` (including nested operands).
Pipeline: boolean (operand UVs kept) → cleanup (degenerate tris removed, UVs
added if absent) → optional `subdivide=N` → normals.

## Normals

- `crease_angle` (degrees, default **60**, finite, 0–180): angle-weighted normals
  are shared across manifold edges whose dihedral angle ≤ this. UV seams share
  the smooth normal but keep their UVs.
- `crease_angle=180`: smooth across all manifold edges.
- `faceted=1`: per-triangle normals. Combining with `crease_angle` is an error.
- `crease_angle` is CSG-only; elsewhere it is an unknown attribute.
- Normal-only changes never move positions, winding, UVs or materials.

## Subdivision (`subdivide=N`)

- Loop subdivision on geometric adjacency (exact float32 positions), not render
  vertices; UV seams are preserved. Masks are convex, so points never leave
  their local support.
- `N` ≤ 3; output capped at 2,000,000 triangles (growth `4^N`).
- Open boundaries need exactly two boundary neighbours per vertex.
- Rejected (error with CSG name and span): non-manifold edges, inconsistent
  winding, disconnected fans, collapsed triangles, skinning, vertex colours.
- No welding, hole filling or remeshing. Slivers with distinct positions are fine.
- Primitive (non-CSG) subdivision keeps render topology; `faceted=1` still applies.

```mog
scene {
  difference "body" (subdivide=1, crease_angle=40) {
    rounded_box (size=[1,0.5,2], radius=0.1)
    box (pos=[0,0.3,0], size=[0.8,0.3,1.6])
  }
}
```
