# MoGen CLI reference

`mogen` is a single static binary. Every entry point is a subcommand —
`mogen <subcommand> …`. Run `mogen --help` for the auto-generated short form,
or `mogen <subcommand> --help` for per-command flags.

This page is the long-form reference. For the language those commands
operate on, see [`dsl.md`](./dsl.md). For the desktop GUI that wraps the
same pipeline, see [`studio.md`](./studio.md).

- [Common flags and conventions](#common-flags-and-conventions)
- [`auth`](#auth) — sign in to Google OAuth + MoGHub
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
- [`moghub`](#moghub) — browse, download, and publish to the MoGHub community
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
| `--style <ps1\|n64\|low-poly\|high-detail\|arcade\|voxel\|cel-shaded\|stylized-fantasy\|cyberpunk\|pixel-art>` | Visual-style hint. Prepends a "## Style" guidance block to the prompt and stamps `meta(style="…")` into the saved DSL. Sticky across `modify` / `animate` / `repair` runs — once stamped, those subcommands inherit the style from the file unless `--style` is passed again to override. Omit to send the prompt unchanged. |
| `--budget_tokens <N>` | Abort if total prompt + response token count exceeds this limit. |
| `--max-repair-iters <N>` | Repair attempts after the first try. Default `2`. |
| `--cached-content <NAME>` | Reuse an existing `cachedContents/...` resource for the system instruction (skips re-uploading the grammar/stdlib reference). |
| `--no-cache` | Disable the automatic system-instruction cache. By default `mogen` creates and reuses a `cachedContents` resource per-binary so repeated calls skip re-upload. |
| `--seed <U64>` | Seed embedded in the DSL header for reproducibility. Defaults to the seed parsed from an existing `.mog`'s header, or a random one if absent. |
| `--dry-run` | Skip GLB compilation and disk writes — print the generated/edited DSL only. |

The seed, the thinking budget, the original prompt, and (when picked) the
visual style are stamped into the top-level `meta(...)` block of every
generated `.mog`:

```mog
meta (
  mogen_version = "0.1.1",
  seed = "1777210527637284168",
  thinking = "high",
  prompt = "A simple four-legged chair.",
  style = "low_poly",
)
```

---

## `auth`

Sign in / out for every credential `mogen` persists under `~/.mogen/`.
The command is target-aware: `mogen auth <target> <verb>` where
`<target>` is one of `gemini-cli`, `antigravity`, or `moghub`. The
top-level `mogen auth status` (no target) prints a one-line summary for
every target at a glance.

```sh
mogen auth status                          # one-line summary per target
mogen auth gemini-cli  {login,status,logout}
mogen auth antigravity {login,status,logout}
mogen auth moghub      {login,status,logout}
```

| target        | what it authenticates                                       | on-disk file                       |
|---------------|-------------------------------------------------------------|------------------------------------|
| `gemini-cli`  | Google OAuth for text gen via Cloud Code Assist             | `~/.mogen/google_auth.json`        |
| `antigravity` | Google OAuth for image gen via Cloud Code Assist            | `~/.mogen/antigravity_auth.json`   |
| `moghub`      | MoGHub session UUID for community publishing + browsing     | `~/.mogen/moghub_auth.json`        |

All three login flows use a loopback browser handshake — `gemini-cli`
and `antigravity` go through Google's OAuth consent screen on a fixed
loopback port (`51121`); `moghub` opens
`<server>/api/auth/desktop/start` and waits for the redirect back. On
Unix the resulting files are written with mode `0600`.

| flag | applies to | meaning |
|---|---|---|
| `--force` | every target's `login` | re-authenticate even if a valid token is already on disk. |
| `--no-browser` | gemini-cli, antigravity | print the authorize URL instead of opening a browser (useful over SSH). |
| `--timeout <SECS>` | gemini-cli, antigravity | how long to wait for the OAuth callback. Clamped to `[10, 3600]`. Default `300`. |
| `--server <URL>` | moghub | sign in against a self-hosted MoGHub instance. The URL round-trips into the on-disk session. |
| `--verbose` | every target's `status` | extra detail — token-store path, OAuth scopes, the chosen `cloudcode-pa` endpoint, and (for `moghub`) a live `whoami` round-trip. |

```sh
mogen auth gemini-cli login                         # zero-config, opens browser
mogen auth antigravity login                        # required for OAuth-driven `mogen textures`
mogen auth moghub login --server https://staging.moghub.org

mogen auth status --verbose                         # show every target with full detail
mogen auth gemini-cli logout                        # scope sign-out per target
```

`mogen generate` / `modify` / `animate` / `repair` automatically use the
gemini-cli OAuth bundle whenever `GEMINI_API_KEY` is unset; `mogen
textures` prefers the antigravity bundle when present (set
`MOGEN_IMAGE_PROVIDER=antigravity` to force it). The MoGHub session
file is shared with Studio, so signing in once via the CLI surfaces the
session in the desktop's Community window and vice versa.

`logout` walks every legacy path (`~/.cache/mogen/`,
`%LOCALAPPDATA%\mogen\`) so a half-cleaned upgrade can't silently
re-authenticate. None of the logout commands call the upstream revoke
endpoint — refresh tokens stay valid server-side until the user
explicitly revokes consent at <https://myaccount.google.com> or signs
out of MoGHub on the web.

---

## `build`

Compile a DSL file to a binary scene container — GLB by default, or FBX
7.4 binary when the output path ends in `.fbx` or `--format fbx` is
passed.

```sh
mogen build <input.mog> [--out <output>] [--format glb|fbx]
```

| flag | meaning |
|---|---|
| `--out`, `-o` | Output path. Defaults to `<input>.glb` (or `<input>.fbx` when `--format fbx` is set). |
| `--format` | Force a specific container, ignoring the extension hint. One of `glb` (default) or `fbx`. |

**Pipeline.** `build` runs the canonical front-to-back pipeline:

1. Parse the DSL (pest grammar).
2. AST validation — referential and typing errors with source spans.
3. Lower to `SceneGraph` — module expansion, placement shortcuts, attach solver, animation/skinning lowering.
4. Graph validation — topological invariants (skeleton ancestry, weight sums, …).
5. Export — PBR materials, embedded textures, animation channels, optional skin data, optional sibling-mesh merge. The format is picked from `--format` (when supplied) or sniffed from the output extension; `.fbx` (case-insensitive) selects FBX, anything else selects GLB.

`generate`, `modify`, `animate`, `repair`, and `textures` all converge on
this command at the end of their flows. If you've already authored a
`.mog`, `build` is what you run.

```sh
mogen build examples/furniture/chair.mog --out chair.glb
mogen build examples/furniture/chair.mog --out chair.fbx          # extension dispatch
mogen build examples/furniture/chair.mog --format fbx             # → chair.fbx
```

### FBX format notes

The FBX exporter emits FBX 7.4 binary that loads in Blender 4.x and other
standard FBX importers. Notable mappings (lossy by FBX's spec):

- PBR materials are encoded as Phong; the raw `metallic` and `roughness`
  factors ride along as `Properties70` entries so PBR-aware importers
  (Blender's principled BSDF) can recover them.
- Quaternion rotations and rotation tracks are converted to Euler XYZ
  degrees; the FBX `RotationOrder` is set to XYZ to match.
- Light intensity passes through verbatim — FBX doesn't distinguish
  candela (point/spot) from lux (directional) like glTF does.
- DSL extras (`role`, `tags`, `cast_shadow`, `kind`) are preserved as
  custom `Properties70` entries on each Model.

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
mogen check examples/furniture/chair.mog                          # human-readable
mogen check examples/furniture/chair.mog --json | jq .            # machine-readable
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
mogen generate "a wooden stool" --style ps1 --out stool.glb       # PS1-style chunky low-poly
mogen modify stool.mog "make it a barstool"                       # inherits meta(style="ps1") automatically
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
mogen modify examples/furniture/chair.mog "make the legs taller"
mogen modify examples/furniture/chair.mog "add armrests" --dsl-out chair_armed.mog --dry-run
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
mogen animate examples/vehicles/drone.mog "spin every rotor at 120 rpm"
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
mogen textures examples/furniture/chair.mog --style "weathered oak"
mogen textures examples/vehicles/drone.mog --no-occlusion --texture-size 512
mogen textures examples/furniture/chair.mog --dry-run                          # see the plan first
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

## `moghub`

Browse, download, like, comment on, and publish to MoGHub — the same
community surface MoGen Studio's Community window exposes, driven from
the terminal. Authentication reads `~/.mogen/moghub_auth.json` written
by [`mogen auth moghub login`](#auth); read-only verbs
(`discover`, `info`, `download`, `comments`) work without a session.
The base URL is taken from the auth file (or `--server`); production
defaults to `https://moghub.org`.

Models are addressed by a `<user>/<slug>` reference (the leading `@` is
optional), e.g. `krazyjakee/parametric-chair` or
`@krazyjakee/parametric-chair`.

Every verb accepts `--server <URL>` to target a self-hosted MoGHub
instance instead of the URL stored in the on-disk session.

```sh
mogen moghub whoami                          # confirm the active session
mogen moghub discover --query chair --kind model --tag furniture
mogen moghub info     @user/cool-stool
mogen moghub download @user/cool-stool --version 3 --out stool/
mogen moghub like     @user/cool-stool
mogen moghub comment  @user/cool-stool "great topology!"
mogen moghub publish  examples/furniture/chair.mog --title "Parametric chair" --tags "chair,furniture"
```

### `moghub whoami`

Print the signed-in user's handle and id. Exits non-zero with the
message `anonymous` if no session is active.

### `moghub discover`

Walk the public discover feed.

| flag | meaning |
|---|---|
| `--query`, `-q` | Free-text search. |
| `--kind` | Filter by kind: `scene`, `model`, `module`, or `all`. |
| `--tag` | Filter by a single tag. |
| `--limit` / `--offset` | Pagination. |
| `--json` | Emit the raw API response as JSON instead of the columnar summary. |

The default human view prints one line per result:
`@user/slug  title  [kind]  ♥like_count  #tag1 #tag2`. A featured pick,
when the API returns one, is shown first prefixed with `★`.

### `moghub info`

Print full detail for a model: kind, license, like + fork counts, the
latest version number, description, tags, and the file list (the entry
`.mog` is marked with `→`).

```sh
mogen moghub info @user/cool-stool
mogen moghub info @user/cool-stool --json
```

### `moghub download`

Fetch a model's `.mog` files into a directory. Defaults to the latest
version; pass `--version <N>` to pin one.

| flag | meaning |
|---|---|
| `--version <N>` | Pin a specific version instead of the latest. |
| `--out`, `-o` | Destination directory. Defaults to `<slug>-v<version>` in the working directory. |
| `--entry-only` | Only fetch the entry `.mog`; skip imports and the thumbnail. |

The thumbnail (`thumbnail.png`) is downloaded best-effort alongside the
`.mog` files unless `--entry-only` is set; versions that were never
thumbnailed silently skip it.

### `moghub comments`

List comments on a model. Soft-deleted comments are hidden. Body
content can include MoGHub bbcode and is printed verbatim.

### `moghub comment`

Post a comment. Requires login. Body accepts MoGHub bbcode.

```sh
mogen moghub comment @user/cool-stool "great topology!"
```

### `moghub like` / `moghub unlike`

Toggle a like on a model. Both verbs are idempotent and print the new
`liked=` / `total=` state. Requires login.

### `moghub notifications`

List the signed-in user's notifications, newest first. Each line is
prefixed with `•` for unread or a space for read entries. Pass
`--mark-read` to mark every notification as read instead of just
listing.

### `moghub publish`

Publish a `.mog` to MoGHub. Bundles the entry `.mog` plus every
locally-imported `.mog` and every referenced PNG / JPG / JPEG / WebP
texture into a single submission. Requires login.

```sh
mogen moghub publish <input.mog> [flags]
```

| flag | meaning |
|---|---|
| `--title` | Override `meta(name=…)` for this publish. Required if the source has no `meta(name=…)`. |
| `--description` | Override `meta(description=…)`. |
| `--tags "a,b,c"` | Comma-separated tag list. Lowercased and capped at 8. Overrides `meta(tags=[…])`. |
| `--license` | SPDX-style license id. Defaults to `CC0-1.0`. |
| `--visibility` | `public`, `unlisted`, or `private`. Defaults to `public`. |
| `--message`, `-m` | Version changelog message. |
| `--thumbnail` | Path to a PNG to attach as the model thumbnail. |
| `--filename` | Override the published filename. Defaults to the input file's basename. |
| `--module` | Publish as a registry-importable module. Mutually exclusive with `--scene`. |
| `--scene` | Publish as a scene. Mutually exclusive with `--module`. |
| `--new` | Force creation of a new model even if the source carries a prior MoGHub stamp. |
| `--server` | Target a self-hosted MoGHub instance. |

**Defaults from `meta(...)`.** When `--title`, `--description`, or
`--tags` are omitted, `publish` reads the corresponding key from the
source's top-level `meta(...)` block. Any locally-imported `.mog`
(via `use "file.mog"`) is bundled automatically; their filenames must
not collide with the entry filename.

**Scene vs. module.** If neither `--module` nor `--scene` is passed,
the source is published as a *module* when it has no `import`
declarations and as a *scene* when it does. Pass an explicit flag to
override.

**Updates round-trip.** On success, `publish` writes three keys back
into the source's `meta(...)` block:

```mog
meta (
  ...
  moghub_model_id = "…",
  moghub_slug     = "…",
  moghub_version  = "2",
)
```

Subsequent `moghub publish` runs read those keys and append a new
version (`moghub_version + 1`) to the same model. Pass `--new` to
ignore the stamp and create a fresh model instead.

**Texture bundling.** Every string attribute that ends in `.png`,
`.jpg`, `.jpeg`, or `.webp` is resolved relative to the `.mog` it
appears in (entry or import) and uploaded. All texture paths must
resolve inside the entry's directory — references that point to a
parent directory are rejected so the upload bundle stays
self-contained.

```sh
mogen moghub publish examples/furniture/chair.mog --title "Parametric chair" --tags "chair,furniture"
mogen moghub publish examples/furniture/chair.mog -m "added armrests"          # appends a new version
mogen moghub publish examples/furniture/chair.mog --new --visibility unlisted  # forks off a fresh model
```

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
