# MoGen CLI reference

`mogen` is a single static binary. Every entry point is a subcommand —
`mogen <subcommand> …`. Run `mogen --help` for the auto-generated short form,
or `mogen <subcommand> --help` for per-command flags.

This page is the long-form reference. For the language those commands
operate on, see [`dsl.md`](./dsl.md). For the desktop GUI that wraps the
same pipeline, see [`studio.md`](./studio.md).

- [Common flags and conventions](#common-flags-and-conventions)
- [`build`](#build) — compile DSL to GLB
- [`parse`](#parse) — dump the AST
- [`check`](#check) — validate a DSL file
- [`dump-scene`](#dump-scene) — print the lowered scene graph
- [`inspect`](#inspect) — summarise a GLB
- [`generate`](#generate) — Gemini-driven scene generation
- [`modify`](#modify) — Gemini-driven edit of an existing `.mog`
- [`animate`](#animate) — Gemini edit limited to animation declarations
- [`repair`](#repair) — auto-fix validation errors with Gemini
- [`textures`](#textures) — generate PBR textures with Gemini Flash Image
- [`bench`](#bench) — run a prompt suite and report success rate
- [Environment variables](#environment-variables)
- [Exit codes](#exit-codes)

---

## Common flags and conventions

A few flag patterns repeat across every LLM-driven subcommand
(`generate`, `modify`, `animate`, `repair`, `textures`, `bench`):

| flag | meaning |
|---|---|
| `--api-key <KEY>` | Override `GEMINI_API_KEY` for this invocation. |
| `--model <NAME>` | Gemini model id. Default `gemini-pro-latest` for text. For `textures`, the default depends on credentials: `gemini-3-pro-image-preview` when authenticated via OAuth (paid plan), otherwise `gemini-2.5-flash-image`. Pass `gemini-3.1-flash-image-preview` (or any other image model) to override. |
| `--temperature <N>` | Sampling temperature. Library default is `0.3` when omitted. |
| `--thinking <low\|medium\|high\|xhigh>` | Cap server-side reasoning. `low` = 512 tokens, `medium` = 2048, `high` = 8192 (default), `xhigh` = 24576 (slowest, most careful). |
| `--budget_tokens <N>` | Abort if total prompt + response token count exceeds this limit. |
| `--max-repair-iters <N>` | Repair attempts after the first try. Default `2`. |
| `--cached-content <NAME>` | Reuse an existing `cachedContents/...` resource for the system instruction (skips re-uploading the grammar/stdlib reference). |
| `--no-cache` | Disable the automatic system-instruction cache. By default `mogen` creates and reuses a `cachedContents` resource per-binary so repeated calls skip re-upload. |
| `--seed <U64>` | Seed embedded in the DSL header for reproducibility. Defaults to the seed parsed from an existing `.mog`'s header, or a random one if absent. |
| `--dry-run` | Skip GLB compilation and disk writes — print the generated/edited DSL only. |

The seed, the thinking budget, and the original prompt are stamped into the
top-level `meta(...)` block of every generated `.mog`:

```mog
meta (
  mogen_version = "0.1.1",
  seed = "1777210527637284168",
  thinking = "high",
  prompt = "A simple four-legged chair.",
)
```

---

## `build`

Compile a DSL file to a GLB.

```sh
mogen build <input.mog> [--out <output.glb>]
```

| flag | meaning |
|---|---|
| `--out`, `-o` | Output GLB path. Defaults to `<input>.glb` alongside the source file. |

**Pipeline.** `build` runs the canonical front-to-back pipeline:

1. Parse the DSL (pest grammar).
2. AST validation — referential and typing errors with source spans.
3. Lower to `SceneGraph` — module expansion, placement shortcuts, attach solver, animation/skinning lowering.
4. Graph validation — topological invariants (skeleton ancestry, weight sums, …).
5. Export GLB — PBR materials, embedded textures, animation channels, optional skin data, optional sibling-mesh merge.

`generate`, `modify`, `animate`, `repair`, and `textures` all converge on
this command at the end of their flows. If you've already authored a
`.mog`, `build` is what you run.

```sh
mogen build examples/chair.mog --out chair.glb
```

---

## `parse`

Parse a DSL file and print the AST. Useful when debugging grammar errors
or checking how a tricky source string lowers.

```sh
mogen parse <input.mog>
```

No GLB is produced. Lowering and validation are skipped.

---

## `check`

Validate a DSL file. Exits non-zero on any error.

```sh
mogen check <input.mog> [--json]
```

| flag | meaning |
|---|---|
| `--json` | Emit diagnostics as line-delimited JSON instead of human-readable carets. |

Validation is dual: AST-level (typing, references, unknown attributes) and
graph-level (topology, weights, skeleton roots). The two phases produce a
unified diagnostic list. Human mode renders via `codespan-reporting`; JSON
mode emits one diagnostic per line in the format the LLM repair loop
consumes.

```sh
mogen check examples/chair.mog                          # human-readable
mogen check examples/chair.mog --json | jq .            # machine-readable
```

---

## `dump-scene`

Lower a DSL file and print the resulting scene graph. Useful for
inspecting what an LLM actually emitted, or for diffing two `.mog` files
post-lowering.

```sh
mogen dump-scene <input.mog> [--json]
```

| flag | meaning |
|---|---|
| `--json` | Emit the graph as JSON instead of an indented summary. |

---

## `inspect`

Read a GLB and print its top-level structure: scenes, meshes, materials,
animations, skins, extensions, embedded image sizes.

```sh
mogen inspect <output.glb>
```

The same machinery that powers MoGen Studio's "GLB summary" panel; useful
for verifying what actually landed in a release artifact.

---

## `generate`

Generate a `.mog` from a natural-language prompt via Gemini, validate it,
repair JSON diagnostics in a loop, then compile it to a GLB.

```sh
mogen generate "<prompt>" [--out <out.glb>] [--dsl-out <out.mog>] [common LLM flags]
```

| flag | meaning |
|---|---|
| `--out`, `-o` | Output GLB path. Ignored with `--dry-run`. |
| `--dsl-out` | Where to stash the generated DSL. Defaults to the sibling of `--out` with a `.mog` extension. Required with `--dry-run` if you want to keep the DSL on disk. |
| `--seed` | Embedded seed; randomised if omitted. |
| `--model` | Gemini model id. Default `gemini-pro-latest`. |
| `--dry-run` | Print the generated DSL but skip compilation and GLB output. |
| Plus all common LLM flags above. |

**Repair loop.** If the generated DSL fails validation, `generate`
re-feeds the JSON diagnostics back to Gemini up to `--max-repair-iters`
times. On the final failure it prints the unfixed diagnostics and exits
non-zero, leaving the broken `.mog` on disk for inspection.

```sh
mogen generate "a wooden stool" --out stool.glb
mogen generate "a clockwork dragon" --thinking xhigh --out dragon.glb
mogen generate "a cube" --thinking low --dry-run                  # no API cost beyond one fast call
```

---

## `modify`

Apply a natural-language edit to an existing `.mog`, then revalidate and
recompile. The model receives the full DSL and your prompt; the response
replaces the file in place (or writes to `--dsl-out`).

```sh
mogen modify <input.mog> "<prompt>" [common LLM flags]
```

| flag | meaning |
|---|---|
| `--out`, `-o` | Output GLB path. Defaults to `<input>.glb`. |
| `--dsl-out` | Where to write the modified DSL. Defaults to modifying `input` in place. |
| `--seed` | Defaults to the seed in the input's header, else random. |
| Plus all common LLM flags. |

The seed embedded in the input header is preserved across edits unless you
override it explicitly. That makes `modify` reproducible for a given
prompt + seed pair.

```sh
mogen modify examples/chair.mog "make the legs taller"
mogen modify examples/chair.mog "add armrests" --dsl-out chair_armed.mog --dry-run
```

---

## `animate`

Same shape as `modify`, but the LLM is restricted to animation top-level
declarations — `joint`, `clip` / `track`, and the procedural templates
(`spin`, `open_close`, `wave`, `flap`, `idle`). Geometry, materials, and
hierarchy are guaranteed not to change.

```sh
mogen animate <input.mog> "<prompt>" [common LLM flags]
```

```sh
mogen animate examples/drone.mog "spin every rotor at 120 rpm"
mogen animate examples/door.mog "make the door swing open over 1.2 seconds"
```

The flag set matches `modify` (`--out`, `--dsl-out`, `--seed`, and the
common LLM flags). Use this when you want the model focused on motion and
not tempted to reshape the scene.

---

## `repair`

Run the validator against an existing `.mog` and ask Gemini to fix every
diagnostic — with the source excerpt, caret, and fix hint passed in. If
the file already validates, `repair` is a no-op success.

```sh
mogen repair <input.mog> [--no-build] [common LLM flags]
```

| flag | meaning |
|---|---|
| `--out`, `-o` | Output GLB path. Defaults to `<input>.glb`. |
| `--dsl-out` | Where to write the repaired DSL. Defaults to in-place. |
| `--no-build` | Stop after rewriting the `.mog`; don't compile the GLB. |
| Plus all common LLM flags. |

Use `repair` after editing a `.mog` by hand and breaking validation, or
after pasting in a snippet from somewhere else. It's the same machinery
the `generate` / `modify` / `animate` repair loops use, exposed as a
top-level command.

---

## `textures`

Generate PBR textures for every material in a `.mog`. The albedo is
LLM-drawn via Gemini 2.5 Flash Image, then locally derived normal,
metallic-roughness, and occlusion maps are computed from the albedo
(Sobel-from-luminance, variance-based, cavity-based). PNGs are written
next to the `.mog` and the matching `*_texture="…"` attrs are spliced
back into the source via spans (no reformatting).

```sh
mogen textures <input.mog> [--style "<hint>"] [--texture-size <N>]
```

| flag | meaning |
|---|---|
| `--out` | Where to write the modified `.mog`. Defaults to in-place. |
| `--glb` | GLB output path. Defaults to `<input>.glb`. |
| `--textures-dir` | Where PNGs are written. Defaults to `textures/<mog-stem>/` so sibling assets don't collide on shared material names. |
| `--style` | Style hint appended to each image prompt. Default `photorealistic`. |
| `--model` | Gemini image model id. Default depends on credentials: `gemini-3-pro-image-preview` when authenticated via OAuth, otherwise `gemini-2.5-flash-image`. Pass `gemini-3.1-flash-image-preview` (or any other image model) to override. |
| `--force` | Regenerate slots whose attr is already declared in the `.mog` or whose PNG already exists at the planned path. |
| `--dry-run` | Print the plan and skip all API calls and file writes. |
| `--no-build` | Stop after rewriting the `.mog`; don't run `build`. |
| `--no-pbr` | Skip every derived PBR map (normal / MR / AO). Albedo is still generated. |
| `--no-normal` / `--no-metallic-roughness` / `--no-occlusion` | Skip a specific derived map. |
| `--texture-size <N>` | Cap (in pixels) on the longer side of every generated albedo. Derived PBR maps inherit this size — the single lever for embedded-texture footprint. `0` keeps the model's native resolution (typically 1024²). |
| `--api-key` | Override `GEMINI_API_KEY`. |

**Idempotency.** Per slot, materials that already declare a given
`*_texture` attr — or whose target PNG already exists at the planned
path — are skipped unless `--force` is passed. Existing on-disk PNGs
still get their `*_texture` attr spliced into the source, just without
an API call or a local re-derivation.

```sh
mogen textures examples/chair.mog --style "weathered oak"
mogen textures examples/drone.mog --no-occlusion --texture-size 512
mogen textures examples/chair.mog --dry-run                          # see the plan first
```

---

## `bench`

Run a suite of prompts through `generate` and report success rate and
mean token cost. Does not write GLBs.

```sh
mogen bench [--prompts <file>] [common LLM flags]
```

| flag | meaning |
|---|---|
| `--prompts` | File with one prompt per line. `#` starts a comment. Defaults to `benches/prompts.txt`. |
| `--model` | Gemini model id. Default `gemini-pro-latest`. |
| `--max-repair-iters` | Default `2`. |
| `--budget-tokens` | Per-prompt token cap. |
| `--api-key` | Override `GEMINI_API_KEY`. |
| `--no-cache` | Disable the system-instruction cache. |
| `--thinking` | Default `high`. |

Used as a regression gate during development — the project targets ≥ 80%
success rate on the bundled prompt suite.

---

## Environment variables

| variable | meaning |
|---|---|
| `GEMINI_API_KEY` | Required by `generate` / `modify` / `animate` / `repair` / `textures` / `bench` unless `--api-key` is passed. |
| `MOGEN_CACHE_DIR` | Where the system-instruction cache is stored. Defaults to `$HOME/.cache/mogen/`. |
| `MOGEN_GOLDENS_UPDATE` | When set during tests, regenerates the golden snapshots used by the validator and exporter test suites. |
| `MOGEN_GLTF_VALIDATOR` | Path to an external glTF validator binary. When set, the build pipeline runs the output GLB through it as an additional smoke check. |

---

## Exit codes

| code | meaning |
|---|---|
| `0` | success |
| non-zero | validation, parse, IO, or remote-API error — diagnostic written to stderr |

`check --json` and the LLM repair loop emit machine-readable diagnostics;
everything else uses human-readable formatting via `codespan-reporting`.
