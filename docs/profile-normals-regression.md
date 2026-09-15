# Profile lighting regression

`sweep`, `loft`, and `extrude` now retain the mesh returned by normal
recomputation. Positions, indices, UVs, cap boundaries and existing vertex
splits are unchanged. Lighting and exported NORMAL bytes intentionally change.
No other discarded `recompute_normals` result was found in the call-site audit.

Review checks (full builds, rendering and golden checks deferred):

```sh
cargo test -p mogen-geom --test profile_normals
cargo test -p mogen-export --test profile_normals
cargo test -p mogen --test goldens
```

For matched-lighting images, build the parent and repaired revisions separately
and run each binary against the **same** absolute path to
`examples/features/profile_normals.mog`:

```sh
mogen thumbnail /absolute/path/profile_normals.mog --out before.png --size 768 --yaw 0.785398 --pitch 0.35
mogen thumbnail /absolute/path/profile_normals.mog --out after.png --size 768 --yaw 0.785398 --pitch 0.35
```

Use the parent binary for `before.png` and the repaired binary for `after.png`,
with the same graphics driver. The sweep, tapered loft and extrusion should
have lit side and cap surfaces after the repair. Geometry and camera framing
must match. Record binary revisions alongside the images. Golden changes for
these primitives should be confined to normals; do not refresh unrelated assets.
