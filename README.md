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
  `prism`, `pyramid`, `disc`, `icosphere`, `rounded_box`
- CSG: `union`, `difference`, `intersect`
- Hierarchy: `group` containers; reusable `module` definitions with `instance`
- Repetition and symmetry: `array`, `mirror`
- Connectors for attachment points between parts
- Skeletons, skinning, and animation templates (`spin`, `open_close`, `wave`, `flap`, `idle`)
- Materials (base color, metallic, roughness, alpha), per-node transforms, `role` and `tags`
- Validation layer with human-readable and JSON diagnostics
- LLM-driven generation via Gemini (`mgen generate`) with automatic repair on failures

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

Two more examples live in [`examples/`](examples/): `hierarchy_test.mg` exercises nested
groups with rotation and scale, `chair_mat.mg` shows the full material flow.

Coordinate system is glTF-standard: right-handed, +Y up, -Z forward.

## CLI

```
mgen build    <file.mg> --out <file.glb>         # compile DSL to GLB
mgen generate "a wooden stool" --out out.glb     # generate DSL via Gemini, then compile
mgen check    <file.mg>                          # validate a DSL file
mgen inspect  <file.glb>                         # summarize a GLB
```

`generate` needs `GEMINI_API_KEY` in the environment (or `--api-key`). There are a few
more developer-facing subcommands (`parse`, `dump-scene`, `bench`) — run `mgen --help`
for the full list.

### Controlling generation latency

Gemini 2.5 Pro does dynamic internal "thinking" before emitting a token — at full budget
that can push a single `generate` call past two minutes. `mgen` exposes a `--thinking`
flag on `generate`, `modify`, and `bench` that caps the budget:

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

## Contributing

Issues and PRs welcome. Good first targets:

- more primitives or parameterized modules in `mgen-geom`
- validation passes in `mgen-dsl/src/lower.rs` (unknown attrs, out-of-range values)
- a second exporter alongside GLB
- snapshot/round-trip tests for the example scenes

Run the test suite with `./test.sh`.

## License

MIT — see [LICENSE](LICENSE).
