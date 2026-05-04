# MoGen — procedural 3D model generator

Website: <https://krazyjakee.github.io/MoGen/>

`mogen` turns a compact, declarative DSL into `.glb` assets. It is designed to be the
deterministic backend of an LLM-driven 3D generation pipeline: the language model writes
high-level structured scenes, `mogen` expands them into real geometry.

Written in Rust. No runtime, no graph editor, no dependencies on a game engine — just a
small parser, a scene graph, a mesh library, and a glTF exporter.

## Why

LLMs are good at structure and intent, bad at floating-point geometry. A DSL like this
one lets the model decide *what* to build — the parts, their roles, how they relate —
while deterministic Rust code handles *how* to build it. Small outputs, cheap iteration,
no hallucinated triangles.

## Status

Usable but still moving fast. Primitives, CSG, hierarchy/modules, arrays and mirrors,
connectors, skeletons + skinning + animation templates, full PBR materials with embedded
textures, validation diagnostics, and LLM-driven generate/modify/animate are all
working. Multiple LLM backends are supported out of the box — Gemini (API key or
Google OAuth), OpenAI, Anthropic, Ollama (local), Claude Code (subscription),
Fireworks AI Firepass (Kimi K2 routers), and Z.ai (GLM family, default `glm-5.1`). See
[`docs/dsl.md`](docs/dsl.md) for the full feature surface.

## Install

### Prebuilt binaries (recommended)

Linux and macOS — one-liner installer:

```sh
curl -fsSL https://raw.githubusercontent.com/krazyjakee/MoGen/master/scripts/install.sh | bash
```

This pulls the latest release from
[GitHub Releases](https://github.com/krazyjakee/MoGen/releases), verifies the SHA-256
checksum, and installs `mogen` and `mogen-studio` into `$HOME/.local/bin`. Run with
`-s -- --help` for options (`--version`, `--bin-dir`, `--cli-only`, `--studio-only`,
`--force`).

Windows — grab the `mogen-<version>-x86_64-pc-windows-msvc.zip` archive from the
releases page and extract it somewhere on your `PATH`.

### Build from source

Requires a recent stable Rust toolchain.

```sh
git clone https://github.com/krazyjakee/MoGen.git
cd MoGen
./scripts/build-release.sh # cargo build --release --workspace
```

The release binary is at `target/release/mogen`. The `./scripts/run-mogen.sh` wrapper runs it via
`cargo run --release` if you'd rather not add it to `$PATH`.

## Quick start

```sh
./scripts/run-mogen.sh build examples/chair.mog --out chair.glb
```

Drop `chair.glb` into Godot, Blender, three.js, or anything else that reads glTF 2.0.

## MoGen Studio

A minimal desktop GUI ships alongside the CLI — **MoGen Studio** combines the DSL
editor, a live 3D preview, diagnostics, and one-click Gemini generate/modify/animate
calls. Build and run it with:

```sh
./scripts/run-studio.sh
```

The inspector panel shows a texture roster for the current scene with a ✓/✗ marker
per texture path, so missing image files are visible before you hit "Build GLB".

## The DSL

Files use the `.mog` extension. Every statement has the same shape — a `kind`, an
optional name, attributes, and optional children:

```
kind "optional name" (attr=value, attr=value, ...) {
  // optional children
}
```

A minimal scene:

```mogen
scene {
  box "seat" (pos=[0, 0.5, 0], size=[1.0, 0.1, 1.0])
  box "back" (pos=[0, 1.0, -0.45], size=[1.0, 1.0, 0.1])
}
```

Coordinate system is glTF-standard: right-handed, +Y up, -Z forward. The full grammar,
every node kind, and worked examples live in [`docs/dsl.md`](docs/dsl.md) and
[`examples/`](examples/).

## CLI

```
mogen build    <file.mog> --out <file.glb>         # compile DSL to GLB
mogen generate "a wooden stool" --out out.glb      # generate DSL via Gemini, then compile
mogen modify   <file.mog> "make the legs taller"   # LLM edit of an existing .mog, then recompile
mogen animate  <file.mog> "spin the rotor at 120 rpm"  # LLM edit limited to animations
mogen check    <file.mog>                          # validate a DSL file
mogen inspect  <file.glb>                          # summarize a GLB
```

`generate`, `modify`, and `animate` need an API key for the chosen provider. By
default that's Gemini (`GEMINI_API_KEY` env var, `--api-key` flag, stored OAuth
bundle, or the shared settings file — see below). Pick a different backend with
`--provider <name>` and the matching env var: `OPENAI_API_KEY`,
`ANTHROPIC_API_KEY`, `FIREWORKS_API_KEY`, `ZAI_API_KEY`, etc. `animate` is
scoped to top-level animation declarations only (`joint`, `clip`/`track`, and
the `spin` / `open_close` / `wave` / `flap` / `idle` templates) — it leaves
geometry, materials, and hierarchy untouched. There are a few more
developer-facing subcommands (`parse`, `dump-scene`, `bench`) — run `mogen --help`
for the full list.

### Shared settings file

Both the CLI and Studio read API keys from `~/.mogen/settings.json`
(`C:\Users\<you>\.mogen\settings.json` on Windows). Studio writes the file as
part of its larger settings struct whenever you save the Preferences panel; the
CLI reads only the API-key fields. Keys entered in Studio therefore satisfy
`mogen generate` etc. on the same machine without also having to export an env
var.

Resolution precedence for every provider is the same: `--api-key` flag → env
var → `~/.mogen/settings.json` → (Gemini only) on-disk OAuth bundle → error.
Set `MOGEN_SETTINGS=/path/to/file.json` to override the path entirely or
`MOGEN_CACHE_DIR=/path/to/dir` to override just the parent directory. Older
installs that wrote settings to `dirs::config_dir()/mogen/settings.json` are
still read on first launch and migrated forward automatically.

The file is plain JSON. A minimal example:

```json
{
  "gemini_api_key": "AIza...",
  "openai_api_key": "sk-...",
  "anthropic_api_key": "sk-ant-...",
  "fireworks_api_key": "fw_...",
  "zai_api_key": "..."
}
```

### Providers

| Provider     | `--provider` value | Default model                              | Env var               |
| ------------ | ------------------ | ------------------------------------------ | --------------------- |
| Gemini       | `gemini`           | `gemini-pro-latest`                        | `GEMINI_API_KEY`      |
| OpenAI       | `openai`           | `gpt-4o`                                   | `OPENAI_API_KEY`      |
| Anthropic    | `anthropic`        | `claude-sonnet-4-5`                        | `ANTHROPIC_API_KEY`   |
| Ollama       | `ollama`           | `llama3.1`                                 | `OLLAMA_API_KEY` \*   |
| Claude Code  | `claude-code`      | `sonnet` (delegates to `claude` CLI)       | — (subscription)      |
| Fireworks AI Firepass | `fireworks` | `accounts/fireworks/routers/kimi-k2p6`     | `FIREWORKS_API_KEY`   |
| Z.ai (GLM)   | `zai`              | `glm-5.1`                                  | `ZAI_API_KEY`         |

\* Ollama is keyless for local installs — the env var is only consulted when
running behind an authenticating reverse proxy.

Notes:

- **Fireworks AI Firepass** ships with the Kimi K2 *Fire Pass* routers
  (`kimi-k2p6` for the thinking model, `kimi-k2p6-turbo` for the fast tier);
  any other Fireworks-hosted model id works via `--model`.
- **Z.ai** uses an OpenAI-compatible Chat Completions endpoint at
  `api.z.ai/api/paas/v4/chat/completions`. The same `ZAI_API_KEY` also
  authenticates the `glm-image` image-gen path used by `mogen textures`.
- **Claude Code** is keyless — it shells out to the `claude` CLI, so auth
  is whatever `claude login` already set up.

### Sign in with a paid Gemini account

Free-tier `GEMINI_API_KEY`s are locked out of `gemini-3-pro-preview` and other
Pro tiers (the API returns `limit=0` 429s). If you have a paid Pro plan via
your Google account, you can sign in with Google instead — no setup required:

```
mogen auth login                    # opens browser, signs in with Google (gemini-cli client)
mogen auth login --antigravity      # signs in with the Antigravity client — required for image gen
mogen auth status                   # show email + project + token expiry
mogen auth logout                   # delete the local tokens
mogen generate "a chair"            # uses OAuth automatically when GEMINI_API_KEY is unset
```

`mogen` ships two public OAuth clients: Google's official
[Gemini CLI](https://github.com/google-gemini/gemini-cli) client (default) and
the Antigravity desktop client (used by Google's Antigravity IDE). Both produce
a normal Google sign-in consent screen. Tokens land in
`~/.mogen/google_auth.json` and `~/.mogen/antigravity_auth.json` respectively
(`C:\Users\<you>\.mogen\…` on Windows) with mode `0600` on Unix. Older installs
that wrote to `~/.cache/mogen/` or `%LOCALAPPDATA%\mogen\` still work — those
paths are read as legacy fallbacks.

Credential precedence on `generate`/`modify`/`animate`/`repair`/`bench` is
`--api-key` flag > `GEMINI_API_KEY` env > `~/.mogen/settings.json` > stored
OAuth token > error. Setting the env var temporarily (or passing `--api-key`)
shadows the OAuth token — `mogen auth status --verbose` flags this when it
happens.

OAuth-mode requests go to `cloudcode-pa.googleapis.com/v1internal:generateContent`
on the user's own Cloud project (discovered via `loadCodeAssist`); the public
`generativelanguage.googleapis.com` API is not used in this mode.

**Caveats.** The bundled OAuth clients are Google's published clients.
They're stable but Google could rotate them; keep `GEMINI_API_KEY` available as
a fallback. The `cachedContents` system-instruction cache is disabled in OAuth
mode (the surface doesn't expose it); the system instruction is sent inline
instead.

### Texture generation

`mogen textures` walks the materials in a `.mog` file and generates per-material
PBR maps. Albedo is generated by an image model; normal, metallic-roughness,
and occlusion maps are derived locally from the albedo (Sobel gradients +
luminance cavity detection, tileable). The generated PNGs land in the
project's `textures/<scene-name>/` folder and the matching
`base_color_texture` / `normal_texture` / etc. attributes are spliced back into
the source file using span-aware edits — no reformatting.

Three image backends are supported:

- **Antigravity OAuth (default for paid Google accounts).** Routes through
  `daily-cloudcode-pa.googleapis.com/v1internal:streamGenerateContent` with
  endpoint failover, model catalog probe, and capacity-error backoff retries
  (3s / 6s / 12s). The plain `mogen auth login` (gemini-cli) bundle is *not*
  accepted by the image surface — run `mogen auth login --antigravity` once.
- **Gemini API key.** When `GEMINI_API_KEY` is set, image gen goes through the
  public `generativelanguage.googleapis.com` surface. Default model is
  `gemini-3-pro-image-preview`; pass `--model gemini-3.1-flash-image-preview`
  (or any other image model) to override.
- **Z.ai `glm-image`.** Selected via Studio's provider dropdown (or the same
  `ZAI_API_KEY` if you wire it up by hand). Useful when you'd rather not burn
  Antigravity quota on bulk texture runs.

Repeat `mogen textures` calls reuse PNGs already on disk (the `build_plan`
step's `UseExisting` action) — only missing slots hit the network. There is no
in-memory or on-disk cache otherwise.

## Contributing

Issues and PRs welcome. Good first targets:

- more primitives or parameterized modules in `mogen-geom`
- validation passes in `mogen-dsl/src/lower.rs` (unknown attrs, out-of-range values)
- a second exporter alongside GLB
- snapshot/round-trip tests for the example scenes

Run the test suite with `./scripts/run-tests.sh`.

## 💖 Support Me
Hi! I’m krazyjakee 🎮, creator and maintain­er of the *NodotProject* - a suite of open‑source Godot tools (e.g. Nodot, Gedis, GedisQueue etc) that empower game developers to build faster and maintain cleaner code.

I’m looking for sponsors to help sustain and grow the project: more dev time, better docs, more features, and deeper community support. Your support means more stable, polished tools used by indie makers and studios alike.

[![ko-fi](https://ko-fi.com/img/githubbutton_sm.svg)](https://ko-fi.com/krazyjakee)

Every contribution helps maintain and improve this project. And encourage me to make more projects like this!

*This is optional support. The tool remains free and open-source regardless.*

## License

MIT — see [LICENSE](LICENSE).
