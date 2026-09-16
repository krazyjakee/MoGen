# MoGen CLI reference

Every entry point is a subcommand: `mogen <subcommand> …`. `mogen --help` and
`mogen <subcommand> --help` print the generated flag lists. The language is in
[`dsl.md`](./dsl.md); the desktop GUI is in [`studio.md`](./studio.md).

- [Common flags and conventions](#common-flags-and-conventions)
- [Providers and credentials](#providers-and-credentials)
- [`auth`](#auth) — sign in to Google OAuth + MoGHub
- [`build`](#build) — compile DSL to GLB or FBX
- [`parse`](#parse) — dump the AST
- [`check`](#check) — validate a DSL file
- [`dump-scene`](#dump-scene) — print the lowered scene graph
- [`import`](#import) — pascalorg/editor scene → `.mog` source
- [`inspect`](#inspect) — summarise a GLB
- [`thumbnail`](#thumbnail) — headless PNG render
- [`pack` / `unpack`](#pack--unpack) — MOGB binary container
- [`generate`](#generate) — AI-driven scene generation
- [`modify`](#modify) — AI-driven edit of an existing `.mog`
- [`session`](#session) — durable generate/refine session with visual review and resume
- [`animate`](#animate) — AI edit limited to animation declarations
- [`repair`](#repair) — auto-fix validation errors with AI
- [`textures`](#textures) — generate PBR textures
- [`bench`](#bench) — run a prompt suite and report success rate
- [`moghub`](#moghub) — browse, download, and publish to MoGHub
- [`mcp`](#mcp) — stdio MCP server
- [`update`](#update) — self-update
- [Environment variables](#environment-variables)
- [Exit codes](#exit-codes)

---

## Common flags and conventions

Flags shared by the LLM subcommands (`generate`, `modify`, `animate`, `repair`, `bench`):

| flag | meaning |
|---|---|
| `--provider <NAME>` | Default `openai`. See [Providers](#providers-and-credentials). |
| `--model <NAME>` | Provider model id. Omitted → provider default (`gpt-6-astra` for OpenAI). |
| `--api-key <KEY>` | Override the selected provider's API key for this call. |
| `--temperature <N>` | Sampling temperature. Omitted → provider default (`0.3` where applicable; not sent to OpenAI reasoning models). |
| `--thinking <low\|medium\|high\|xhigh>` | Reasoning effort. OpenAI: `reasoning.effort`. Gemini: `low`=512, `medium`=2048, `high`=8192, `xhigh`=24576 tokens. Ignored by Ollama. Falls back to the file's `meta(thinking=…)` (modify/animate/repair), then `high`. |
| `--style <ps1\|n64\|low-poly\|high-detail\|arcade\|voxel\|cel-shaded\|stylized-fantasy\|cyberpunk\|pixel-art>` | Visual-style hint; stamps `meta(style=…)`. `modify`/`animate`/`repair` inherit the file's style unless overridden. |
| `--budget-tokens <N>` | Abort if prompt + response tokens exceed this. |
| `--max-repair-iters <N>` | Repair attempts after the first try. Default `2`. |
| `--cached-content <NAME>` | Gemini only: reuse a `cachedContents/...` resource for the system instruction. |
| `--no-cache` | Gemini only: disable the automatic system-instruction cache (stored under `MOGEN_CACHE_DIR`). |
| `--seed <U64>` | Seed stamped into the DSL header. Defaults to the input file's seed, else random. |
| `--dry-run` | Print the DSL only; no compilation or disk writes. |

Generated files carry a top-level `meta(...)` block recording seed, thinking level,
prompt, and style:

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

## Providers and credentials

| Provider | `--provider` | Default model | Credential |
|---|---|---|---|
| OpenAI (default) | `openai` | `gpt-6-astra` (fast tier `gpt-5-mini`) | `OPENAI_API_KEY` |
| Gemini | `gemini` (API key), `auto`, `gemini-oauth`, `antigravity` | `gemini-pro-latest` | `GEMINI_API_KEY` or OAuth |
| Anthropic | `anthropic` | `claude-sonnet-4-5` | `ANTHROPIC_API_KEY` |
| Ollama | `ollama` | `llama3.1` | none locally; `OLLAMA_API_KEY` only behind an auth proxy |
| OpenAI Codex | `codex` | `gpt-6-astra` | subscription via `codex login` |
| Claude Code | `claude-code` | `sonnet` | subscription via `claude` CLI login |
| Fireworks AI Firepass | `fireworks` | `accounts/fireworks/routers/kimi-k2p6` (fast `kimi-k2p6-turbo`) | `FIREWORKS_API_KEY` |
| Z.ai (GLM) | `zai` | `glm-5.1` | `ZAI_API_KEY` |

Gemini provider values: `gemini` uses the API key only; `gemini-oauth` uses the
`gemini-cli` OAuth bundle; `antigravity` uses the Antigravity bundle; `auto` tries
flag → env → settings → gemini-cli OAuth → Antigravity OAuth.

**Key resolution:** `--api-key` → env var → `~/.mogen/settings.json` → (Gemini only)
stored OAuth token → error. An env var or `--api-key` shadows the OAuth token;
`mogen auth status --verbose` flags this.

**Shared settings file.** CLI and Studio share `~/.mogen/settings.json`
(`C:\Users\<you>\.mogen\settings.json` on Windows), so keys saved in Studio's
Preferences work for the CLI. `MOGEN_SETTINGS` overrides the file path;
`MOGEN_CACHE_DIR` overrides its directory. Older locations are read and
migrated automatically. The CLI reads only the key fields:

```json
{
  "gemini_api_key": "AIza...",
  "openai_api_key": "sk-...",
  "anthropic_api_key": "sk-ant-...",
  "fireworks_api_key": "fw_...",
  "zai_api_key": "..."
}
```

**Codex subscription.** Install a Codex CLI with `--ignore-user-config` support and run
`codex login` (check with `codex login status`). Then pass `--provider codex`. Codex
handles login and token refresh; MoGen stores no tokens, uses your plan limits, records
zero per-call API cost, and never falls back to an API key. Reference images work;
texture generation uses its own image provider.

**Claude Code** shells out to the `claude` CLI; auth is whatever `claude login` set up.

**Z.ai** uses an OpenAI-compatible Chat Completions endpoint; `ZAI_API_KEY` also
authenticates `glm-image` for `textures`.

**Gemini on a paid Google account.** Free-tier API keys get `limit=0` 429s on Pro
models. Sign in with Google instead via [`mogen auth`](#auth). OAuth requests go to
`cloudcode-pa.googleapis.com` on your own Cloud project; the `cachedContents` cache is
unavailable in that mode. The bundled OAuth clients are Google's public clients and
could be rotated, so keep an API key as a fallback.

---

## `auth`

Sign in / out for credentials stored under `~/.mogen/`.

```sh
mogen auth status [--verbose]              # one-line summary per target
mogen auth gemini-cli  {login,status,logout}
mogen auth antigravity {login,status,logout}
mogen auth moghub      {login,status,logout}
```

| target | authenticates | file |
|---|---|---|
| `gemini-cli` | Google OAuth, text gen via Cloud Code Assist | `~/.mogen/google_auth.json` |
| `antigravity` | Google OAuth, image gen via Cloud Code Assist (required for OAuth `textures`) | `~/.mogen/antigravity_auth.json` |
| `moghub` | MoGHub session | `~/.mogen/moghub_auth.json` |

Logins use a loopback browser handshake (Google on port `51121`; MoGHub via
`<server>/api/auth/desktop/start`). Files are mode `0600` on Unix. The MoGHub
session is shared with Studio's Community window.

| flag | applies to | meaning |
|---|---|---|
| `--force` | `login` | Re-authenticate even if a valid token exists. |
| `--no-browser` | gemini-cli, antigravity | Print the authorize URL instead of opening a browser. |
| `--timeout <SECS>` | gemini-cli, antigravity | Callback wait, clamped to `[10, 3600]`. Default `300`. |
| `--server <URL>` | moghub | Self-hosted MoGHub instance; stored in the session. |
| `--verbose` | `status` | Token-store path, scopes, chosen endpoint, and (moghub) a live `whoami`. |

```sh
mogen auth gemini-cli login
mogen auth moghub login --server https://staging.moghub.org
mogen auth gemini-cli logout
```

`logout` also clears legacy paths (`~/.cache/mogen/`, `%LOCALAPPDATA%\mogen\`), which
are still read as fallbacks. It does not revoke tokens server-side; revoke at
<https://myaccount.google.com> or sign out of MoGHub on the web.

---

## `build`

Compile a DSL file to GLB (default) or FBX 7.4 binary.

```sh
mogen build <input.mog> [--out <output>] [--format glb|fbx]
```

| flag | meaning |
|---|---|
| `--out`, `-o` | Output path. Defaults to `<input>.glb` (or `.fbx` with `--format fbx`). |
| `--format` | `glb` or `fbx`. Otherwise an `.fbx` output extension (case-insensitive) selects FBX. |

Pipeline: parse → AST validation → lower to scene graph → graph validation → export.
The LLM subcommands finish by running this.

```sh
mogen build examples/furniture/chair.mog --out chair.glb
mogen build examples/furniture/chair.mog --format fbx             # → chair.fbx
```

### FBX format notes

Loads in Blender 4.x and standard FBX importers. Lossy mappings:

- PBR materials become Phong; `metallic` / `roughness` are kept as `Properties70` entries.
- Rotations and rotation tracks become Euler XYZ degrees (`RotationOrder` XYZ).
- Light intensity passes through verbatim (no candela/lux distinction).
- DSL extras (`role`, `tags`, `cast_shadow`, `kind`) become custom `Properties70` entries.

---

## `parse`

Print the AST. No lowering, validation, or output file.

```sh
mogen parse <input.mog>
```

---

## `check`

Validate a DSL file (AST and graph phases). Exits non-zero on any error.

```sh
mogen check <input.mog> [--json]
```

`--json` emits one diagnostic per line — the format the LLM repair loop consumes.

---

## `dump-scene`

Lower a DSL file and print the resulting scene graph.

```sh
mogen dump-scene <input.mog> [--json]
```

---

## `import`

Convert a [pascalorg/editor](https://github.com/pascalorg/editor) scene into editable
`.mog` **source** (not a GLB).

```sh
mogen import <scene.json> [--out <house.mog>] [--name <scene-name>]
```

| flag | meaning |
|---|---|
| `--out` | Output path. Defaults to `<input>.mog` beside the JSON. |
| `--name` | Name of the emitted `scene`. Defaults to the output file stem. |

Walls, slabs, ceilings, roofs and openings become geometry; furniture and fixtures
become POI markers. Doors and windows become the hole plus a posed group wrapping
`use "door_simple"` / `use "window_simple"`. Unsupported node kinds (MEP, cabinets,
sheets, guides) are reported, never fatal.

Everything the importer skipped, dropped, or corrected is written into the output
header as a comment:

```
// Imported from a pascalorg/editor scene as "gatehouse".
// 26 walls · 3 slabs · 3 ceilings · 2 roof segments · 12 markers
// Skipped 2 nodes: site (1), guide (1)
//
// 7 item(s) needed attention:
//   wall 25: curves tighter than its own thickness, dropped
//   slab_2: HoleOutsideOuter, dropped
//   roof 0: eave sat 0.800m below the walls under it; raised to meet them
//   …
```

`dropped` means the shape (e.g. a self-intersecting polygon or a hole outside its outer
ring) would have produced non-watertight geometry. A roof below the walls it covers is
lifted onto them and reported. `examples/buildings/gatehouse.pascal.json` is a worked
example with deliberate defects (see that folder's `README.md`).

---

## `inspect`

Print a GLB's structure: scenes, meshes, materials, animations, skins, extensions,
embedded image sizes.

```sh
mogen inspect <output.glb>
```

---

## `thumbnail`

Render a PNG preview of a `.mog` with the headless renderer (suitable for
`moghub publish --thumbnail`).

```sh
mogen thumbnail <input.mog> [--out preview.png]
```

| flag | meaning |
|---|---|
| `--out`, `-o` | Defaults to `<input>.png`. |
| `--size <PX>` | Square edge length. Default `512`. |
| `--yaw <RAD>` | Camera yaw. Default π/4. |
| `--pitch <RAD>` | Camera pitch. Default `0.5`. |
| `--bg <HEX>` | Background, e.g. `#2a2d33`. Default slate grey. |

Also writes a `<out>.camera.json` sidecar (convention version, source/dependency revision,
camera parameters). Asset front follows `meta(front=…)` ([dsl.md](./dsl.md#file-metadata-meta)).

---

## `pack` / `unpack`

Experimental MOGB binary container: a compact encoding of the parsed `.mog`. Text
`.mog` stays canonical.

```sh
mogen pack   <input.mog>  [--out <file.mogb>] [--lossy]
mogen unpack <input.mogb> [--out <file.mog>]
```

| flag | meaning |
|---|---|
| `pack --out`, `-o` | Defaults to `<input>.mogb`. |
| `pack --lossy` | `/1000` fixed-point numbers (smaller; lossy past 3 decimals). Default is lossless. |
| `unpack --out`, `-o` | Write source here instead of stdout. |

---

## `generate`

Generate a `.mog` from a prompt, validate it, repair diagnostics in a loop, then compile.

```sh
mogen generate "<prompt>" [--out <out.glb>] [--dsl-out <out.mog>] [common LLM flags]
```

| flag | meaning |
|---|---|
| `--out`, `-o` | Output GLB path. Ignored with `--dry-run`. |
| `--dsl-out` | DSL path. Defaults to `--out` with a `.mog` extension; needed with `--dry-run` to keep the DSL. |
| `--plan` | Run an Architect pass that writes a Markdown plan first, then generate DSL from it. One extra LLM call. |
| `--auto-refine <N>` | Render the result and let a vision-capable model critique and revise it `N` times (0–10, default `0`). |

If validation still fails after `--max-repair-iters`, the diagnostics are printed, the
broken `.mog` is left on disk, and the exit code is non-zero.

```sh
mogen generate "a wooden stool" --out stool.glb
mogen generate "a clockwork dragon" --thinking xhigh --out dragon.glb
mogen generate "a cube" --thinking low --dry-run
mogen generate "a wooden stool" --style ps1 --out stool.glb
```

---

## `modify`

Apply a natural-language edit to an existing `.mog`, then revalidate and recompile.

```sh
mogen modify <input.mog> "<prompt>" [common LLM flags]
```

| flag | meaning |
|---|---|
| `--out`, `-o` | Output GLB. Defaults to `<input>.glb`. |
| `--dsl-out` | Where to write the DSL. Defaults to editing in place. |
| `--plan` / `--auto-refine <N>` | As for `generate`. |
| `--rewrite` | Make the model re-emit the whole file instead of SEARCH/REPLACE edit blocks (which already fall back to a rewrite when they don't apply). |

The input's seed is preserved unless `--seed` overrides it.

```sh
mogen modify examples/furniture/chair.mog "make the legs taller"
mogen modify examples/furniture/chair.mog "add armrests" --dsl-out chair_armed.mog --dry-run
```

---

## `session`

Durable generate → build → five matched views → typed review → scoped
Inspect/Measure/Apply/Render corrections, using the same runner as Studio's
[Generate and Refine](./studio.md#generate-and-refine). Build failures go through the
same tool loop.

```sh
mogen session --prompt "<brief>" --out-dir <new-dir> [flags]
mogen session --brief brief.json --out-dir <new-dir> [flags]
mogen session --input chair.mog --prompt "Thicken the arms" --lock seat --selected-part arm --out-dir <new-dir>
mogen session --resume --out-dir <dir> [--provider …] [--glb]
mogen session --inspect --out-dir <dir>
```

| flag | meaning |
|---|---|
| `--out-dir` | Must be new, except with `--resume` / `--inspect` (which require a saved session). |
| `--prompt` | Brief for a new asset, or a correction appended to `--input`'s saved brief. |
| `--brief <json>` | `ModelingBrief` (style, dimensions, intended use, required details, constraints). Replaces the saved brief; conflicts with `--prompt`. |
| `--input <mog>` | Refine an existing asset into `--out-dir`. Its sidecar supplies brief, references and locks. |
| `--resume` | Continue with saved limits, brief, references, selection, locks, model and sampling. Rejects `--prompt/--brief/--model/--seed/--temperature/--thinking/--reference/--selected-part/--lock`. |
| `--inspect` | Print saved state and artifact paths; no calls. |
| `--provider`, `--model` | Default provider `openai`. Keys are never saved. Refined mode needs an image-capable model. |
| `--seed`, `--temperature`, `--thinking low\|medium\|high\|xhigh` | Optional sampling. |
| `--draft` | Generate/build only; no review or GL. Not with `--input`. |
| `--reference <png>` | Repeatable; embedded in the sidecar with digests. |
| `--selected-part`, `--lock <part>` | Focus edits on / lock a uniquely named part (locks repeatable, merged with saved locks). |
| `--calls` / `--iterations` / `--seconds` | Limits; defaults `12` / `3` / `600`. |
| `--spend-usd` | Spend cap; stops if pricing is unknown. Subscription cost reports as unknown, not $0. |
| `--output-tokens` | Per-call output cap, default `16384`. |
| `--glb` | Also export `final.glb`. |
| `--experimental-guidance` | Technique-retrieval shape guidance (not a default). |
| `--script <json>` | Credential-free fake provider: JSON array of response strings. |

Rendering: Linux needs EGL/OpenGL 3.3 (headless: `EGL_PLATFORM=surfaceless
LIBGL_ALWAYS_SOFTWARE=1` with Mesa `libegl1 libegl-mesa0 libgl1-mesa-dri`); macOS/Windows
need a graphical session. A GL probe runs before any model call. Plain `generate`/`modify`
(and `--auto-refine`) are unchanged.

**Output.** Stdout is one JSON report (progress on stderr); Ctrl-C cancels. Report fields
include absolute `final_mog` (null if it doesn't match the selected snapshot), `selected_mog`,
`latest_mog`, `sidecar`, `report`, selected/latest revisions, `stop_reason`, usage, elapsed
time. The directory holds `final.mog`, optional `final.glb`, `final.mog.modeling.json`,
`report.json`, `generation-response.txt`, `candidate-*`, and `revision-N/` full
source/dependency snapshots. Input dependencies may not use those names. Candidates record
whether they were reviewed/selected.

| status | meaning |
|---|---|
| `completed` | Draft done, reviewer finished, or no useful edit; see `stop_reason`. Not a quality guarantee. |
| `budget_exhausted` | Call/iteration/time/spend limit hit; work is saved. |
| `canceled` | Cancelled; late responses may be saved. |
| `review_format_failed` | Format repair failed; raw responses kept in the journal. |
| `render_unavailable` | GL or image capability missing. |
| `stopped` | Other provider/validation/stale-state/protocol error. |

**Resume rules.** Sidecar v2 (loads v1 read-only; v1 has no transcript) saves atomically.
Provider responses are journalled before parsing; Apply results and captures are saved
immediately. Resume replays saved responses/captures without re-sending calls or re-applying
edits, and restores charged usage and elapsed time. Changed source, dependencies, brief,
model/provider, selection or locks reject resume. A call interrupted in flight is counted as
one reserved call of unknown cost and may be billed twice. Journal cap: 256 responses or
32 MiB; responses over 1 MiB are saved but not interpreted.

**Review format.** JSON `{findings: string, complete: bool, improved: bool, correction:
string}`; a fenced object or an array of findings strings is normalised, missing booleans are
not defaulted, and one format-only retry must preserve the original judgments. A bare/fenced
whole `mog` document is accepted as a full-source Apply.

MCP exposes `session` with the same parameters and returns the report as `structuredContent`.

---

## `animate`

Like `modify`, but the model may only change animation declarations — `joint`,
`clip` / `track`, and the templates `spin`, `open_close`, `wave`, `flap`, `idle`.
Geometry, materials, and hierarchy are untouched. Flags: `--out`, `--dsl-out`, plus the
common LLM flags.

```sh
mogen animate examples/vehicles/drone.mog "spin every rotor at 120 rpm"
```

---

## `repair`

Validate an existing `.mog` and ask the model to fix every diagnostic (with source
excerpt, caret, and fix hint). No-op success if the file already validates.

```sh
mogen repair <input.mog> [--no-build] [common LLM flags]
```

| flag | meaning |
|---|---|
| `--out`, `-o` | Output GLB. Defaults to `<input>.glb`. |
| `--dsl-out` | Defaults to in place. |
| `--no-build` | Stop after rewriting the `.mog`. |

---

## `textures`

Generate PBR textures for every material. Albedo comes from an image model; normal,
metallic-roughness, and occlusion maps are derived locally from it (tileable). The
`*_texture="…"` attributes are spliced into the source without reformatting.

```sh
mogen textures <input.mog> [--style "<hint>"] [--texture-size <N>]
```

| flag | meaning |
|---|---|
| `--out` | Where to write the `.mog`. Defaults to in place. |
| `--glb` | GLB output. Defaults to `<input>.glb`. |
| `--textures-dir` | PNG directory relative to the `.mog`. Default `textures/<mog-stem>/`. |
| `--style` | Style hint for each image prompt. Default `photorealistic`. |
| `--model` | Image model. Default `gemini-3.1-flash-image` when signed in via `mogen auth antigravity login`, else `gemini-2.5-flash-image`. |
| `--api-key` | Override `GEMINI_API_KEY`. |
| `--zai-api-key` | Use Z.ai `glm-image` for albedo (or set `ZAI_API_KEY`). |
| `--force` | Regenerate slots already declared or already on disk. |
| `--dry-run` | Print the plan; no API calls or writes. |
| `--no-build` | Skip the GLB build. |
| `--no-pbr` | Skip all derived maps (albedo only). |
| `--no-normal` / `--no-metallic-roughness` / `--no-occlusion` | Skip one derived map. |
| `--texture-size <N>` | Max albedo edge in pixels (derived maps match). Default `512`; `0` keeps native size (typically 1024²). |

Image backends: Antigravity OAuth (preferred when signed in; the `gemini-cli` bundle is
not accepted), a Gemini API key, or Z.ai `glm-image`. Capacity errors retry with backoff.

Slots whose attr is already declared, or whose PNG already exists, are skipped unless
`--force`; existing PNGs are still spliced into the source. There is no other cache.

```sh
mogen textures examples/furniture/chair.mog --style "weathered oak"
mogen textures examples/vehicles/drone.mog --no-occlusion --texture-size 512
mogen textures examples/furniture/chair.mog --dry-run
```

---

## `bench`

Run prompts through `generate` and report success rate and mean token cost. Writes no
GLBs. The project targets ≥ 80% success on the bundled suite.

```sh
mogen bench [--prompts <file>] [common LLM flags]
```

`--prompts`: one prompt per line, `#` comments; default `benches/prompts.txt`.
Also accepts `--provider`, `--model`, `--max-repair-iters`, `--budget-tokens`,
`--api-key`, `--no-cache`, `--thinking` (default `high`).

---

## `moghub`

Browse, download, like, comment on, and publish to MoGHub from the terminal. Write verbs
(`publish`, `comment`, `like`/`unlike`, `notifications`) need
[`mogen auth moghub login`](#auth). The server comes from the session file, default
`https://moghub.org`; every verb accepts `--server <URL>`. Models are referenced as
`<user>/<slug>` (leading `@` optional).

```sh
mogen moghub whoami
mogen moghub discover --query chair --kind model --tag furniture
mogen moghub info     @user/cool-stool [--json]
mogen moghub download @user/cool-stool --version 3 --out stool/
mogen moghub comments @user/cool-stool
mogen moghub comment  @user/cool-stool "great topology!"
mogen moghub like     @user/cool-stool      # also: unlike
mogen moghub notifications [--mark-read]
mogen moghub publish  examples/furniture/chair.mog --title "Parametric chair" --tags "chair,furniture"
```

- **`whoami`** — handle and id; exits non-zero (`anonymous`) with no session.
- **`discover`** — `--query`/`-q`, `--kind` (`scene`/`model`/`module`/`all`), `--tag`,
  `--limit`, `--offset`, `--json`. Prints `@user/slug  title  [kind]  ♥likes  #tags`; a
  featured pick is shown first with `★`.
- **`info`** — kind, license, likes/forks, latest version, description, tags, files
  (entry marked `→`).
- **`download`** — `--version <N>` (default latest), `--out`/`-o` (default
  `<slug>-v<version>`), `--entry-only` (skip imports and `thumbnail.png`).
- **`comments`** — hides soft-deleted comments; bbcode printed verbatim.
- **`comment`** — body accepts MoGHub bbcode.
- **`like` / `unlike`** — idempotent; prints `liked=` / `total=`.
- **`notifications`** — newest first, `•` marks unread; `--mark-read` marks all read.

### `moghub publish`

Bundles the entry `.mog`, every locally imported `.mog`, and referenced PNG / JPG /
JPEG / WebP textures.

| flag | meaning |
|---|---|
| `--title` | Overrides `meta(name=…)`; required if absent. |
| `--description` | Overrides `meta(description=…)`. |
| `--tags "a,b,c"` | Overrides `meta(tags=[…])`. Lowercased, max 8. |
| `--license` | SPDX id. Default `CC0-1.0`. |
| `--visibility` | `public` (default), `unlisted`, `private`. |
| `--message`, `-m` | Version changelog message. |
| `--thumbnail` | PNG thumbnail. Omitted → rendered headlessly. |
| `--filename` | Published filename. Default: input basename. |
| `--module` / `--scene` | Publish kind (mutually exclusive). Default: scene if the file has imports, else module. |
| `--new` | Create a new model even if the file has a MoGHub stamp. |

On success `publish` writes `moghub_model_id`, `moghub_slug`, and `moghub_version` into
`meta(...)`; later publishes append a new version to that model unless `--new`.
Imported `.mog` filenames must not collide with the entry filename. Texture paths
resolve relative to the `.mog` they appear in and must stay inside the entry's
directory.

```sh
mogen moghub publish examples/furniture/chair.mog -m "added armrests"          # new version
mogen moghub publish examples/furniture/chair.mog --new --visibility unlisted  # new model
```

---

## `mcp`

Run `mogen` as an MCP server over stdio (JSON-RPC). Every other subcommand is exposed as
a tool; each call runs the same binary in CLI mode. Launch it from an MCP client, not
interactively.

---

## `update`

Self-update `mogen` (and a sibling `mogen-studio`) from the latest GitHub release. By
default only checks and prints what it would do.

| flag | meaning |
|---|---|
| `--yes` | Install without prompting. |
| `--check` | Print the latest release tag and exit. |
| `--force` | Reinstall even if already current. |

---

## Environment variables

| variable | meaning |
|---|---|
| `OPENAI_API_KEY`, `GEMINI_API_KEY`, `ANTHROPIC_API_KEY`, `OLLAMA_API_KEY`, `FIREWORKS_API_KEY`, `ZAI_API_KEY` | Provider keys (see [Providers](#providers-and-credentials)). |
| `MOGEN_SETTINGS` | Full path of the shared settings file. |
| `MOGEN_CACHE_DIR` | Directory for caches, settings, and auth files (defaults: caches `$HOME/.cache/mogen/`, settings/auth `~/.mogen/`). |
| `MOGEN_TOKEN_STORE`, `MOGEN_ANTIGRAVITY_TOKEN_STORE`, `MOGEN_MOGHUB_SESSION_STORE` | Full-path overrides for the three auth files. |
| `MOGEN_GOLDENS_UPDATE` | In tests, regenerate golden snapshots. |
| `MOGEN_GLTF_VALIDATOR` | Path to an external glTF validator run on output GLBs. |

---

## Exit codes

| code | meaning |
|---|---|
| `0` | success |
| non-zero | validation, parse, IO, or remote-API error — diagnostic on stderr |
