# mgen — procedural 3D model generator

`mgen` turns a compact, declarative DSL into `.glb` assets. It is designed to be the
deterministic backend of an LLM-driven 3D generation pipeline: the language model writes
high-level structured scenes, `mgen` expands them into real geometry.

Written in Rust. No runtime, no graph editor, no dependencies on a game engine — just a
small parser, a scene graph, a mesh library, and a glTF exporter.

## Status

Usable but still moving fast. The DSL parses, the scene graph builds, primitives render,
and GLB export works. Most of the original ambitions in [`PLAN.md`](PLAN.md) have landed.

Supported:

- Primitives: `box`, `plane`, `quad`, `cylinder`, `cone`, `sphere`, `capsule`, `torus`,
  `prism`, `pyramid`, `disc`, `icosphere`, `rounded_box`, `wedge`, `frustum`, `tube`,
  `hemisphere`, `half_cylinder`, `torus_arc`, `ellipsoid`, `superellipsoid`,
  `curved_plane`, `lathe`, `spline_tube` — each emits `TEXCOORD_0` UVs
- CSG: `union`, `difference`, `intersect`, with post-op triplanar UVs so booleans stay
  texturable
- Hierarchy: `group` containers; reusable `module` definitions with `instance`
- Repetition and symmetry: `array`, `mirror`
- Connectors for attachment points between parts
- Skeletons, skinning, and animation templates (`spin`, `open_close`, `wave`, `flap`, `idle`)
- Materials: base color, metallic, roughness, alpha, transmission, emissive + HDR
  strength, `double_sided`, per-node transforms, `role` and `tags`
- PBR textures: `base_color_texture`, `metallic_roughness_texture`, `normal_texture`,
  `occlusion_texture`, `emissive_texture`. PNGs and JPEGs are embedded in the output
  GLB, so `.glb` files remain self-contained and portable.
- Validation layer with human-readable and JSON diagnostics
- LLM-driven generation, modification, and animation via Gemini (`mgen generate` /
  `mgen modify` / `mgen animate`) with automatic repair on failures

## Install

Requires a recent stable Rust toolchain.

```sh
git clone <this-repo>
cd godot-model-gen
./build.sh         # cargo build --release --workspace
```

The release binary is at `target/release/mgen`. The `./mgen.sh` wrapper runs it via
`cargo run --release` if you'd rather not add it to `$PATH`.

## Quick start

```sh
./mgen.sh build examples/chair.mg --out chair.glb
```

Drop `chair.glb` into Godot, Blender, three.js, or anything else that reads glTF 2.0.

## The DSL

Files use the `.mg` extension. The shape of every statement is the same:

```
kind "optional name" (attr=value, attr=value, ...) {
  // optional children
}
```

A minimal scene:

```mg
scene {
  box "seat" (pos=[0, 0.5, 0], size=[1.0, 0.1, 1.0])
  box "back" (pos=[0, 1.0, -0.45], size=[1.0, 1.0, 0.1])
}
```

With materials, grouping, and metadata:

```mg
material "wood"   (color=[0.55, 0.35, 0.18], metallic=0.0, roughness=0.75)
material "fabric" (color=[0.20, 0.30, 0.55], metallic=0.0, roughness=0.95)

scene {
  group "chair" (pos=[0, 0, 0], role="furniture", tags="chair,seat") {
    box      "seat" (pos=[0, 0.5, 0],     size=[1.0, 0.1, 1.0], mat="fabric", role="seat")
    box      "back" (pos=[0, 1.0, -0.45], size=[1.0, 1.0, 0.1], mat="wood",   role="back")
    cylinder "leg_fl" (pos=[-0.45, 0.25, -0.45], radius=0.05, height=0.5, mat="wood", role="leg")
    cylinder "leg_fr" (pos=[ 0.45, 0.25, -0.45], radius=0.05, height=0.5, mat="wood", role="leg")
    cylinder "leg_bl" (pos=[-0.45, 0.25,  0.45], radius=0.05, height=0.5, mat="wood", role="leg")
    cylinder "leg_br" (pos=[ 0.45, 0.25,  0.45], radius=0.05, height=0.5, mat="wood", role="leg")
  }
}
```

With textures:

```mg
material "oak" (
  color=[1, 1, 1],
  roughness=0.8,
  base_color_texture="textures/oak_albedo.png",
  normal_texture="textures/oak_normal.png"
)

scene {
  box "table_top" (pos=[0, 0.75, 0], size=[2, 0.05, 1], mat="oak")
}
```

Texture paths are resolved relative to the `.mg` file and the image bytes are embedded
into the output `.glb`, so the result is a single portable file with no external
dependencies.

Two more examples live in [`examples/`](examples/): `hierarchy_test.mg` exercises nested
groups with rotation and scale, `chair_mat.mg` shows the full material flow.

Coordinate system is glTF-standard: right-handed, +Y up, -Z forward.

## CLI

```
mgen build    <file.mg> --out <file.glb>         # compile DSL to GLB
mgen generate "a wooden stool" --out out.glb     # generate DSL via Gemini, then compile
mgen modify   <file.mg> "make the legs taller"   # LLM edit of an existing .mg, then recompile
mgen animate  <file.mg> "spin the rotor at 120 rpm"  # LLM edit limited to animations
mgen check    <file.mg>                          # validate a DSL file
mgen inspect  <file.glb>                         # summarize a GLB
```

`generate`, `modify`, and `animate` need `GEMINI_API_KEY` in the environment (or
`--api-key`). `animate` is scoped to top-level animation declarations only (`joint`,
`clip`/`track`, and the `spin` / `open_close` / `wave` / `flap` / `idle` templates) —
it leaves geometry, materials, and hierarchy untouched. There are a few more
developer-facing subcommands (`parse`, `dump-scene`, `bench`) — run `mgen --help`
for the full list.

### Controlling generation latency

Gemini 2.5 Pro does dynamic internal "thinking" before emitting a token — at full budget
that can push a single `generate` call past two minutes. `mgen` exposes a `--thinking`
flag on `generate`, `modify`, `animate`, and `bench` that caps the budget:

| level    | budget tokens | when to use                                                  |
|----------|---------------|--------------------------------------------------------------|
| `low`    | 512           | fast path; simple, unambiguous prompts                       |
| `medium` | 2048          | slightly ambiguous prompts                                   |
| `high`   | 8192          | default; balances latency with planning for complex scenes   |
| `xhigh`  | 24576         | near-max quality; expect ~2 min on Pro                       |

```sh
./mgen.sh generate "a wooden stool" --out stool.glb                  # high (default)
./mgen.sh generate "a simple cube"   --thinking low                  # cheaper, faster
./mgen.sh generate "a clockwork dragon" --thinking xhigh             # slower, most careful
```

## Why

LLMs are good at structure and intent, bad at floating-point geometry. A DSL like this
one lets the model decide *what* to build — the parts, their roles, how they relate —
while deterministic Rust code handles *how* to build it. Small outputs, cheap iteration,
no hallucinated triangles.

The long-form design goals live in [`PLAN.md`](PLAN.md).

## GUI

A minimal desktop GUI ships alongside the CLI — it combines the DSL editor, a live
3D preview, diagnostics, and one-click Gemini generate/modify/animate calls. Build
and run it with:

```sh
cargo run --release -p mgen-gui
```

The inspector panel shows a texture roster for the current scene with a ✓/✗ marker
per texture path, so missing image files are visible before you hit "Build GLB".

## Contributing

Issues and PRs welcome. Good first targets:

- more primitives or parameterized modules in `mgen-geom`
- validation passes in `mgen-dsl/src/lower.rs` (unknown attrs, out-of-range values)
- a second exporter alongside GLB
- snapshot/round-trip tests for the example scenes

Run the test suite with `./test.sh`.

## License

MIT — see [LICENSE](LICENSE).
