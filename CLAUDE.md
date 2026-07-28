# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project

`mogen` (brand: **MoGen**) is a Rust CLI that compiles a compact declarative DSL (`.mog` files)
into glTF 2.0 `.glb` assets. It is designed as the deterministic backend of an LLM-driven 3D
generation pipeline: an LLM writes high-level structured scenes, `mogen` expands them into real
geometry. Primary engine target is Godot 4.x, but glTF output must remain spec-compliant.

The desktop GUI is **MoGen Studio** (crate `mogen-studio`, binary `mogen-studio`).

## Commands

```sh
./scripts/build-release.sh                  # cargo build --release --workspace
./scripts/run-tests.sh                      # cargo test --workspace
./scripts/run-mogen.sh <subcommand> …       # cargo run --release --bin mogen -- …
./scripts/run-studio.sh                     # cargo run --release -p mogen-studio

# one package / one test
cargo test -p mogen-dsl
cargo test -p mogen-geom csg::tests::difference_basic -- --exact

# common CLI flows
./scripts/run-mogen.sh build    examples/furniture/chair.mog --out chair.glb
./scripts/run-mogen.sh check    examples/furniture/chair.mog [--json]      # validate; exits non-zero on errors
./scripts/run-mogen.sh parse    examples/furniture/chair.mog               # dump AST
./scripts/run-mogen.sh dump-scene examples/furniture/chair.mog --json      # dump lowered SceneGraph
./scripts/run-mogen.sh inspect  chair.glb                        # read back + summarize a GLB
./scripts/run-mogen.sh import   scene.json --out house.mog       # pascalorg/editor JSON → .mog SOURCE, not a GLB
./scripts/run-mogen.sh generate "a wooden stool" --out stool.glb     # Gemini-driven; needs GEMINI_API_KEY
./scripts/run-mogen.sh modify   examples/furniture/chair.mog "make legs taller" # LLM edit of an existing .mog
./scripts/run-mogen.sh bench    --prompts benches/prompts.txt         # ≥80% success gate

# GUI
./scripts/run-studio.sh                     # MoGen Studio desktop app
```

`generate`/`modify`/`bench` read `GEMINI_API_KEY` from env (or take `--api-key`). `generate` and
`modify` stamp `meta(seed=…, thinking=…, prompt=…)` into the output `.mog` so rebuilds are
reproducible and the per-file thinking budget / original prompt round-trip across edits.

## Architecture

Cargo workspace under `crates/`. The compile pipeline is a strict layering; keep cross-crate
dependencies pointing in one direction:

```
mogen-dsl  ──parse──►  AST  ──validate_ast──►  lower  ──►  mogen-core::SceneGraph
                                                              │
                                                              ├──validate_graph──►
                                                              │
                                                              └──mogen-export──►  .glb
```

- **mogen-core** — pure data: `SceneGraph` (arena of `SceneNode` with parent/child ids),
  `Transform` (glam-based TRS), `Mesh`, `Material`, `Connector` (pos + quat + tag + optional
  radius), `Joint`/`Clip`/`Track` for animation, `Skin` for skinning, `Diagnostic`/`Severity`/
  `Span` for error reporting, and `Aabb` helpers. No I/O, no parsing.
- **mogen-dsl** — pest grammar (`grammar.pest`), AST (`ast.rs`), parser, and the lowering
  pipeline that turns AST → `SceneGraph`. Lowering is split across files by concern:
  `module.rs` (module declarations + `use` expansion with `$param` substitution, recursion
  detection, expansion cache), `lower.rs` (geometry/materials/transforms/CSG/mirror/array),
  `attach.rs` (connector frame alignment), `anim_lower.rs` (joints, clips, procedural
  templates), `skin_lower.rs` (skeletons, bones, automatic weight binding). Every AST node
  preserves pest spans — diagnostics depend on this. `lower/arch/` is a separate layer with
  its own rules — see **Architectural IR** below.
- **mogen-validate** — two-phase validator. `validate_ast` runs on the parsed AST (unknown
  kinds, missing/typo attrs, unknown references). `validate_graph` runs on the lowered
  `SceneGraph` (topology, weights summing to 1, skeleton-root ancestry, etc.). Both produce
  `Diagnostic` values; `render_human` uses `codespan-reporting`, `render_json` emits the
  line-delimited format the LLM repair loop consumes.
- **mogen-geom** — primitives (`box`, `cylinder`, `cone`, `sphere`, `capsule`, `torus`,
  `prism`, `pyramid`, `disc`, `icosphere`, `rounded_box`, `plane`, `quad`), CSG via the
  `manifold-csg` crate (zmerlynn) wrapping Google's Manifold C++ library
  (`union`/`difference`/`intersect` with many-arg variants), mesh transforms, and cleanup
  (vertex welding, degenerate-tri cull, normal recomputation). CSG ops call `clean_csg_output`
  to give the exporter a watertight mesh. Two mutually-exclusive Cargo features select the
  Manifold build flavour: `csg` (default — native cmake build, used by all desktop crates)
  and `unstable-wasm-uu` (cross-compile to `wasm32-unknown-unknown` via `wasm-cxx-shim`,
  used by `mogen-wasm`; requires LLVM 20+ on the build host — see README).
- **mogen-anim** — procedural animation templates (`spin`, `open_close`, `wave`, `flap`,
  `idle`) that build `Clip`s. v1 emits glTF node-transform tracks only; skinning lives in
  `mogen-core::Skin` + exporter and is driven by the same joint nodes.
- **mogen-export** — hand-rolled GLB writer (JSON chunk + BIN chunk). Uses `serde_json` for
  the JSON side; buffer packing is manual via `to_le_bytes`. Writes PBR materials, animation
  channels, and skins (`skins[]`, `JOINTS_0`/`WEIGHTS_0` accessors, `node.skin` refs). The
  `asset.generator` field in the output GLB is `"MoGen"`. `options.rs` defines
  `ExportOptions` (`include_animations`, `include_textures`, `merge_sibling_meshes`) consumed
  by `write_glb_with_options`; `merge.rs` is an optional pre-export pass that CSG-unions
  same-material, non-skinned sibling leaf meshes into one node (preserves hierarchy,
  animations, skins, connectors — but drops per-vertex UVs on merged groups, so textured
  meshes fall back to flat PBR when merged).
- **mogen-llm** — Gemini `generateContent` client (`gemini.rs`), system-instruction assembly
  from grammar + stdlib index + examples (`prompt.rs`), and the repair loop (`repair.rs`)
  that re-feeds JSON diagnostics for up to `max_repair_iters` retries. `embed_seed_header` /
  `parse_seed_header` keep the seed round-tripping through the DSL file. A separate PBR
  texture pipeline lives in `textures.rs` + `pbr_maps.rs` + `image.rs`: `textures.rs` walks
  the AST, generates per-material albedo PNGs via Gemini 2.5 Flash Image with a fresh
  per-call random seed, and splices `texture = "…"` attributes back into the source using
  spans (no reformatting). `pbr_maps.rs` derives normal / metallic-roughness / occlusion maps
  locally from the albedo (Sobel gradients + luminance cavity detection, tileable). Image
  generation retries on `IMAGE_RECITATION` up to 3×. There is no in-memory or on-disk cache
  for generated images — `build_plan`'s `UseExisting` action handles repeat builds by
  reusing PNGs already on disk in the project's `textures/` folder.
- **mogen** — the binary; `clap` subcommands (`build`, `parse`, `check`, `dump-scene`,
  `inspect`, `generate`, `modify`, `bench`). `build` is the canonical pipeline and the other
  LLM commands end by calling it.
- **mogen-studio** — the desktop GUI (eframe/egui). Reuses the same pipeline as the CLI via
  `pipeline.rs`, adds a live 3D preview (`viewer.rs`), and calls Gemini through `mogen-llm`.
  Window title is "MoGen Studio"; settings live at `~/.config/mogen/settings.json`. Per-file
  state carries `ExportOptions` and `TextureUiConfig` so mesh-merge and texture choices stick
  across tabs. Studio is split into focused modules:
  - `edit.rs` — span-aware source mutations (`set_attr`, `delete_node`) that preserve
    formatting/diagnostics; used by the inspector and gizmo drags.
  - `gizmo.rs` — pure-math translate/rotate/scale handles with hit-testing + drag logic
    (viewport GL drawing stays in `viewer.rs`).
  - `highlight.rs` — loose tokenizer → `LayoutJob` syntax colouring. Intentionally
    independent of the pest parser so mid-edit source still colours.
  - `pick.rs` — screen-space ray cast (Möller–Trumbore) mapping clicks to `NodeId`s.
  - `theme.rs` — five colour-scheme presets (Dark, Light, Sunset, Nord, HighContrast)
    persisted by label and applied to egui visuals.

## Architectural IR (`lower/arch/`)

A shared vocabulary for buildings — walls as **centrelines** (start/end + thickness + optional
sagitta arc), slabs as polygons with holes, roofs as a type plus a pitch — modelled on
pascalorg/editor's data model (MIT). Two producers fill in an `ArchModel`: `mogen-pascal`'s
importer, and the `building` generator. One solver (`resolve::solve`) turns it into shapes, and
two sinks emit those as either `.mog` source (`sink/mog_text.rs`) or meshes (`sink/mesh.rs`).

**The rule that makes it worth having: producers only map fields; every piece of geometry maths
lives in `arch/`.** A mitre solved in a producer is a mitre the next producer has to write again.

Invariants, each enforced by test:

- **No RNG.** Nothing here may reach `lower::rng`. Ties break by deterministic index, and ids
  are `Vec` indices precisely so the solver never needs a hash map.
- **Watertight by construction.** Every output shape is a closed solid; there is no
  open-surface variant for a sink to mishandle. `sink/mesh.rs` builds prisms by ear-clipping
  rather than calling `extrude_mesh`, which returns a capless tube on a self-intersecting ring
  and reports nothing.
- **Junctions are covered exactly once.** Mitred wedges tile the ring around a junction, never
  its middle, so `miter` patches any junction whose wedges leave area — a four-way crossing
  otherwise leaves a full-height column of nothing.
- **Openings are filled, not just cut.** A door or window leaves a `resolved::OpeningInstance` —
  a pose and a size, no shape — which `sink/mog_text.rs` renders as a posed group wrapping
  `use "door_simple"` / `use "window_simple"` (and declares the materials those modules bind, or
  the file fails to lower). The solver decides *where* the doorway is; what a door looks like
  stays swappable in the emitted source. `Passage` and `Niche` stay holes.
- **A roof cannot sink into its walls.** `roof::supported_top` finds the walls a segment covers —
  the whole centreline inside the eave rectangle, which is what stops a porch roof being hoisted
  by the two-storey wall it abuts — and lifts the eave onto them, with a warning. Left where it
  was put, every wall poking out through the slopes is still a valid closed solid, so nothing
  downstream can tell it from a design.

The `building` generator gets a narrower verb than the importer does: `solve_wall_meshes` takes
centrelines plus the frame each mesh is wanted in, and returns geometry only. Node names,
transforms, POI anchors and furniture slots stay with the generator, which is why retargeting it
was a change to geometry rather than to everything. `building/tests/parity.rs` holds that line
across 48 configurations.

## Procedural generators

The node kinds `branch` (plants/trees), `building`, `cave`, `terrain`, and `dungeon` are a
distinct class of DSL node: a single declaration expands into a whole subtree of geometry. They
are not ordinary geometry nodes — each is a deterministic *function of its attributes*, and they
all share one infrastructure layer so they look and behave identically in the CLI and in Studio.
Live under `crates/mogen-dsl/src/lower/` (`branch.rs` is a single file; the rest are directories
with `mod.rs` / `config.rs` / `emit.rs` / `materials.rs` / `poi.rs`). Dispatch happens by node
kind in `lower/node.rs`.

What makes them special, and the contracts every generator must honour:

- **Seed-driven determinism.** Each reads a `seed=` attr (floored at 1) and draws from the shared
  LCG in `lower/rng.rs`. `mix_seed(base, salt)` derives independent sub-streams per phase
  (layout / emit / decorate) so adding a draw in one phase never perturbs another. Same seed +
  same attrs ⇒ byte-identical geometry. Never introduce nondeterminism (HashMap iteration order,
  wall-clock, thread races) into a generator.
- **Editable wrapper + frozen subtree.** Every generator follows
  `begin_procedural()` → emit subtree → `finish_procedural()` (`lower/procedural.rs`). The wrapper
  node carries the user's transform/span/metadata and stays `editable=true`; *everything* emitted
  beneath it is stamped `editable=false`, because a rebuild from the seed would wipe any hand-edit.
  Studio's pick logic redirects clicks on frozen geometry up to the editable wrapper.
- **Schema-driven UI, zero per-kind Studio code.** Editable parameters are declared once as a
  `ProcSchema` in `crates/mogen-dsl/src/proc_schema.rs` (per-param type, range, enum options,
  group, hover help). Studio renders the inspector grid generically from that schema
  (`app/ui_panels/selected/geom_params.rs`) — there are no hand-written per-generator UI arms.
  Adding a generator (or a param) means extending the schema, not the GUI. A drift-guard test
  asserts every enum option still lowers. Studio has no explicit "regenerate" button: editing an
  attr rewrites the `.mog` source, recompiles, and the whole subtree is re-emitted.
- **POI markers as the gameplay interface.** Generators emit transform-only "points of interest"
  (`lower/poi.rs`, `emit_poi_group()`) under a `points_of_interest` group — e.g. cave entrances,
  building furniture slots, dungeon spawn/treasure/stairs. Each carries `role` + `tags` that
  export to glTF `node.extras`, so the engine can find and populate them. POIs are deterministic
  and **LOD-invariant** (same anchors at every detail level); optionally visualised as coloured
  debug spheres when `debug_show_poi=1`.
- **Shared support layer.** Config reading (`lower/cfg.rs`: `seed`/`flag`/`count`/`scalar` with
  consistent clamping), scoped material defaults (`ensure_named_defaults()` — user-declared names
  win over generator defaults, scoped by file origin), and LOD scaling (`lower/lod.rs` guards that
  compound a file-level `lod_scale` with per-node `lod=`) are centralised so all five stay
  consistent. Keep new generators on these helpers rather than re-rolling RNG/config/material/POI
  logic locally.

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
- Branding: use **MoGen** / **MoGen Studio** in prose and user-facing UI; lowercase
  `mogen` / `mogen-studio` for crate names, binary names, env vars, and path identifiers.
- **Studio UI text size**: never call `.small()` on `RichText` / `egui::Label` for any
  user-facing copy (captions, hints, status lines, helper text, pricing notes, dialog
  subtitles, etc.). Body text must render at the default `TextStyle::Body` size — egui's
  small style is unreadable at our default scale. For de-emphasis use `.weak()` alone
  (same size, dimmer colour). Compact button affordances like
  `egui::Button::new("▲").small()` are the only allowed `.small()` use, and only when the
  button is icon/glyph-only.
- Environment variables are `MOGEN_CACHE_DIR`, `MOGEN_GOLDENS_UPDATE`, `MOGEN_GLTF_VALIDATOR`.
  Caches default to `$HOME/.cache/mogen/`.

## Reference docs

- `docs/dsl.md` — authoritative DSL surface (every node kind, attribute, expression form).
- `docs/ROADMAP.md` — milestones M1–M10, ordering constraints, and risks worth respecting
  when extending the language.
- `docs/modules.md` — stdlib module catalog.
- `examples/*.mog` — canonical usage of each feature (hierarchy, materials, array/mirror, CSG,
  modules, connectors/attach, animation, skeletons). `tests/broken/*.mog` covers diagnostic
  snapshots.
- `examples/buildings/gatehouse.pascal.json` — a whole building in pascalorg/editor's format,
  kept as the importer's main regression case and driven by
  `crates/mogen-pascal/tests/real_project.rs` (not a golden). Generated by
  `crates/mogen-pascal/tests/fixtures/make_gatehouse.py` — edit the script, not the JSON.
  `examples/buildings/README.md` explains the four defects it plants on purpose, and why it
  must never import cleanly.
- MoGHub web community. The actual repo lives at `../moghub` (sibling to this checkout); cross-repo work happens there.

## Git

- **Never use `git stash`** unless the user explicitly requests it. Stashed work is easy to
  forget and silently drops uncommitted changes from the working tree. If you need a clean
  tree to perform some operation, ask the user how to proceed (commit, branch, or abort)
  rather than stashing.
