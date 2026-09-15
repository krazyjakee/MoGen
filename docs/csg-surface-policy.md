# CSG shading and subdivision

Boolean results (`union`, `difference`, `intersect`, including nested operands)
now receive a final normal pass after cleanup and optional subdivision:

1. Boolean evaluation preserves operand UV attributes.
2. Cleanup removes degenerate CSG triangles and supplies UVs when absent.
3. `subdivide=N` computes Loop positions using **geometric adjacency**, independent
   of duplicated UV/shading vertices. UV interpolation retains render seams.
4. Angle-weighted normals join adjacent faces across manifold edges whose
   dihedral angle is at most `crease_angle` degrees. UV seams share the smooth
   normal without merging their different UV coordinates.

For normals only, position matching tolerates up to twice float32 epsilon times
the mesh's largest extent (minimum `1e-12`). This joins roundoff at analytic UV
seams, such as `sin(2π)` on a sphere, while leaving stored positions untouched.
Subdivision uses exact positions instead, as described below.

The default is **60 degrees**. This intentionally replaces the old averaging
of normals over arbitrary render-index connectivity. `crease_angle=180` requests
smooth normals across all manifold edges; it is not a byte-for-byte legacy
shading mode. `crease_angle=40` matches the sports-body repair experiment, but
is not a universal optimum. `faceted=1` produces per-triangle normals. Combining
it with `crease_angle` is an error. Angles must be finite and between 0 and 180.
`crease_angle` is CSG-specific; other kinds receive the normal unknown-attribute
diagnostic. Primitive `faceted=1` also remains effective after subdivision.

Normal-only changes leave every triangle's float32 positions, winding and
per-corner UV/material semantics unchanged. They can split render vertices;
compiled geometry locks correctly include the changed normals. Connectors and
bounds remain based on the same surface. Preview and GLB use the same compiled
mesh; no external mesh-normal conversion or inline `poly` rewrite is needed.

## Subdivision input contract

The sports body has 28,358 render vertices but 6,578 exact geometric positions.
The render-index mesh has boundary valences 2, 4, 6 and 8; its geometric mesh is
closed and edge-manifold. The old boundary rule used `.75*self + .125*sum`,
even for more than two boundary neighbors, so weights exceeded one and moved
points outside their local support. This caused the observed spikes.

The new CSG path constructs exact float32 position adjacency (signed zero is
canonicalized), checks edges and connected vertex fans, and applies positive
Loop masks whose weights sum to one. Every new point stays in the convex hull
of its local support; repeated iterations cannot expand the input's convex
hull. Open boundaries require exactly two boundary neighbors. Non-manifold
edges, inconsistent winding, disconnected fans, collapsed geometric triangles, skinning and vertex
colors are rejected with the CSG declaration's name and byte span. Tiny/sliver
triangles with distinct positions remain supported. No tolerance welding,
automatic hole filling or remeshing occurs.

Levels are capped at 3 by existing lowering rules, with an additional limit of
2 million output triangles checked before allocation. Growth is `4^N`.
Unsupported inputs fail compilation before Studio/Apply can replace a valid
candidate. Ordinary primitive subdivision retains its render topology; its
malformed boundary fallback now leaves such vertices stationary.

Credential-free tests: `cargo test -p mogen-geom --lib` and
`cargo test -p mogen --test sports_body -- --nocapture`. The second checks the
actual compact [body fixture](../benches/quality/targets/sports_body.mog), levels
0–2, normals, preserved corner attributes and native GLB export. See the
[milestone validation report](../benches/quality/reliability/README.md) for measured mesh sizes, timings and captures.

This does not remove unrelated W1208 zero-area warnings, solve UV tiling, or
establish printability or production quality for arbitrary generated models.
