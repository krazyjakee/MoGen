# Renderable mesh contract

CLI checks, Studio compilation, session compilation, GLB/FBX export and
headless captures share `mogen_core::validate_renderable_scene`. Export checks
both the incoming scene and merge output. Preview rejects malformed scenes
before flattening or graphics upload. Validation is read-only.

| Code | Meaning and correction |
|---|---|
| E1200 | Channel length mismatch. Supply one entry per position, including normals for meshes with triangles; omit an optional channel entirely if unused. |
| E1201 | Non-finite transform, non-unit quaternion or unusable normal matrix. Correct local/ancestor transforms and remove collapsed scale. |
| E1202 | Non-finite local or world position. Correct generator/import parameters; finite bounds do not exempt individual vertices. |
| E1203 | Incomplete triangle or out-of-range index. Repair the index stream. |
| E1204 | Non-finite normal, or a zero/non-unit normal on a rendered face. Generate finite unit normals in the constructor/importer, retaining hard edges and UV seams. |
| E1205 | Non-finite UV. Repair the UV generator. |
| E1206 | Non-finite vertex colour. Repair the colour generator. |
| E1207 | Invalid skin streams, bindings or inverse-bind matrices. Supply matching, finite channels and valid references. |
| W1208 | Zero-area triangles; advisory, retained unchanged. Check repeated points/collapsed dimensions. |
| W1209 | No triangles; advisory. Export retains the node but omits its surface. |
| E1210 | Invalid/cyclic/duplicate/unreachable scene links. Repair graph references. |

Diagnostics aggregate counts per part/channel, and carry authored names,
node numbers, source spans and imported filenames when lowering retains them.
Synthesized nodes without source spans still identify the compiled part.
`MeshContractError` retains structured diagnostics through exporter/session
errors; it can also be downcast from `anyhow::Error`.

## Compatibility and numerical policy

- Open surfaces, intentional UV seams, split normals and disconnected props
  are renderable. Existing connectivity policy remains a separate graph check.
- UVs, colours and skin channels may be absent. Present channels must match
  position count; joint and weight channels must be paired with a valid skin.
- Meshes with triangles require a full normal channel. Missing normals must
  be generated at construction/import, where smoothing intent is known.
  Validation/export never guess smoothing or repair geometry silently.
- Normals on nondegenerate rendered faces must have length 1 within 0.001.
  Non-finite values are errors even at unused vertices. Zero normals at unused
  or exclusively zero-area vertices are allowed.
- Degeneracy uses an exactly zero cross product computed in f64 from f32
  coordinates. It has no fixed scene-unit tolerance that would erase tiny
  legitimate faces. Degenerate faces remain advisory, with no watertightness
  or fabrication guarantee.
- Negative scales remain supported. Singular transforms on surfaces are
  rejected because normal-matrix inversion cannot produce finite values.
- Checks scan vertices, indices and hierarchy linearly; diagnostics are
  aggregated rather than emitting an error for every corrupt vertex.

## Review

The focused core contract tests are independent of GL or CSG. Broader checks
are intentionally left for review:

```sh
cargo test -p mogen-core --test renderable
cargo test -p mogen-export --test mesh_contract --test profile_normals
cargo test -p mogen-validate mesh_contract
cargo test -p mogen-studio mesh_contract
cargo test -p mogen-render --test mesh_contract
cargo run -p mogen --example quality_eval -- --out /tmp/mesh-contract-review --no-render
```

The quality evaluator records `mesh_contract` separately from dimensions,
required parts and human visual judgment. Its `mesh-contract-controls.json`
records deliberate zero/missing normals, NaN positions/UVs and invalid indices,
the original source revision, mutation identity, expected codes and observed
diagnostics. A missed control fails the evaluation. Run the evaluator with
rendering enabled during review, and use the matched-lighting fixture in
[profile-normals-regression.md](profile-normals-regression.md). These controls
establish deterministic geometry checks, not live-provider quality.
