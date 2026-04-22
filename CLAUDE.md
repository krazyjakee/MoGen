# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project

`mgen` is a Rust CLI that compiles a compact declarative DSL (`.mg` files) into glTF 2.0 `.glb`
assets. It is designed as the deterministic backend of an LLM-driven 3D generation pipeline: an
LLM writes high-level structured scenes, `mgen` expands them into real geometry. Primary engine
target is Godot 4.x, but glTF output must remain spec-compliant.

## Commands

```sh
./build.sh                                  # cargo build --release --workspace
./test.sh                                   # cargo test --workspace
./mgen.sh <subcommand> …                    # cargo run --release --bin mgen -- …

# one package / one test
cargo test -p mgen-dsl
cargo test -p mgen-geom csg::tests::difference_basic -- --exact

# common CLI flows
./mgen.sh build    examples/chair.mg --out chair.glb
./mgen.sh check    examples/chair.mg [--json]      # validate; exits non-zero on errors
./mgen.sh parse    examples/chair.mg               # dump AST
./mgen.sh dump-scene examples/chair.mg --json      # dump lowered SceneGraph
./mgen.sh inspect  chair.glb                       # read back + summarize a GLB
./mgen.sh generate "a wooden stool" --out stool.glb     # Gemini-driven; needs GEMINI_API_KEY
./mgen.sh modify   examples/chair.mg "make legs taller" # LLM edit of an existing .mg
./mgen.sh bench    --prompts benches/prompts.txt        # ≥80% success gate
```

`generate`/`modify`/`bench` read `GEMINI_API_KEY` from env (or take `--api-key`). `generate` and
`modify` embed a `// mgen-generate seed=…` header so rebuilds are reproducible.

## Architecture

Cargo workspace under `crates/`. The compile pipeline is a strict layering; keep cross-crate
dependencies pointing in one direction:

```
mgen-dsl  ──parse──►  AST  ──validate_ast──►  lower  ──►  mgen-core::SceneGraph
                                                             │
                                                             ├──validate_graph──►
                                                             │
                                                             └──mgen-export──►  .glb
```

- **mgen-core** — pure data: `SceneGraph` (arena of `SceneNode` with parent/child ids),
  `Transform` (glam-based TRS), `Mesh`, `Material`, `Connector` (pos + quat + tag + optional
  radius), `Joint`/`Clip`/`Track` for animation, `Skin` for skinning, `Diagnostic`/`Severity`/
  `Span` for error reporting, and `Aabb` helpers. No I/O, no parsing.
- **mgen-dsl** — pest grammar (`grammar.pest`), AST (`ast.rs`), parser, and the lowering
  pipeline that turns AST → `SceneGraph`. Lowering is split across files by concern:
  `module.rs` (module declarations + `use` expansion with `$param` substitution, recursion
  detection, expansion cache), `lower.rs` (geometry/materials/transforms/CSG/mirror/array),
  `attach.rs` (connector frame alignment), `anim_lower.rs` (joints, clips, procedural
  templates), `skin_lower.rs` (skeletons, bones, automatic weight binding). Every AST node
  preserves pest spans — diagnostics depend on this.
- **mgen-validate** — two-phase validator. `validate_ast` runs on the parsed AST (unknown
  kinds, missing/typo attrs, unknown references). `validate_graph` runs on the lowered
  `SceneGraph` (topology, weights summing to 1, skeleton-root ancestry, etc.). Both produce
  `Diagnostic` values; `render_human` uses `codespan-reporting`, `render_json` emits the
  line-delimited format the LLM repair loop consumes.
- **mgen-geom** — primitives (`box`, `cylinder`, `cone`, `sphere`, `capsule`, `torus`,
  `prism`, `pyramid`, `disc`, `icosphere`, `rounded_box`, `plane`, `quad`), CSG via `csgrs`
  (`union`/`difference`/`intersect` with many-arg variants), mesh transforms, and cleanup
  (vertex welding, degenerate-tri cull, normal recomputation). CSG ops call `clean_csg_output`
  to give the exporter a watertight mesh.
- **mgen-anim** — procedural animation templates (`spin`, `open_close`, `wave`, `flap`,
  `idle`) that build `Clip`s. v1 emits glTF node-transform tracks only; skinning lives in
  `mgen-core::Skin` + exporter and is driven by the same joint nodes.
- **mgen-export** — hand-rolled GLB writer (JSON chunk + BIN chunk). Uses `serde_json` for
  the JSON side; buffer packing is manual via `to_le_bytes`. Writes PBR materials, animation
  channels, and skins (`skins[]`, `JOINTS_0`/`WEIGHTS_0` accessors, `node.skin` refs).
- **mgen-llm** — Gemini `generateContent` client (`gemini.rs`), system-instruction assembly
  from grammar + stdlib index + examples (`prompt.rs`), and the repair loop (`repair.rs`)
  that re-feeds JSON diagnostics for up to `max_repair_iters` retries. `embed_seed_header` /
  `parse_seed_header` keep the seed round-tripping through the DSL file.
- **mgen** — the binary; `clap` subcommands (`build`, `parse`, `check`, `dump-scene`,
  `inspect`, `generate`, `modify`, `bench`). `build` is the canonical pipeline and the other
  LLM commands end by calling it.

## Conventions

- Coordinate system is glTF-standard: right-handed, +Y up, -Z forward.
- Math everywhere is `glam` (`Vec3`, `Quat`, `Mat4`, `Affine3A`).
- Connectors are oriented frames (`pos + Quat + tag`), not points. Never reduce them to
  position-only — every stdlib module and the attach solver assume orientation.
- Modules (`module "name" (p=default) { … }` + `use "name" (p=v)`) are first-class grammar
  productions with their own AST node and lexical `$param` scope. Do not implement them as
  string substitution.
- Validation is dual by design: referential/typing errors on AST (with spans); geometric/
  topological on lowered `SceneGraph` (with node → AST back-refs so spans survive). Keep the
  two passes separate.
- Animation in v1 is **node-transform tracks only**. Skinning (`Skin`, `JOINTS_0`,
  `WEIGHTS_0`) is separate and additive — joints are still scene nodes.
- LLM repair loop is bounded (`max_repair_iters`, default 2), always uses JSON diagnostics,
  and embeds a seed in the DSL header for reproducibility.

## Reference docs

- `docs/dsl.md` — authoritative DSL surface (every node kind, attribute, expression form).
- `docs/ROADMAP.md` — milestones M1–M10, ordering constraints, and risks worth respecting
  when extending the language.
- `docs/modules.md` — stdlib module catalog.
- `examples/*.mg` — canonical usage of each feature (hierarchy, materials, array/mirror, CSG,
  modules, connectors/attach, animation, skeletons). `tests/broken/*.mg` covers diagnostic
  snapshots.
