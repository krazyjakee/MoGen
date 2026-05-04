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
textures, validation diagnostics, and Gemini-driven generate/modify/animate are all
working. See [`docs/dsl.md`](docs/dsl.md) for the full feature surface.

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

`generate`, `modify`, and `animate` need `GEMINI_API_KEY` in the environment (or
`--api-key`). `animate` is scoped to top-level animation declarations only (`joint`,
`clip`/`track`, and the `spin` / `open_close` / `wave` / `flap` / `idle` templates) —
it leaves geometry, materials, and hierarchy untouched. There are a few more
developer-facing subcommands (`parse`, `dump-scene`, `bench`) — run `mogen --help`
for the full list.

### Sign in with a paid Gemini account (Antigravity OAuth)

Free-tier `GEMINI_API_KEY`s are locked out of `gemini-3-pro-preview` and other
Pro tiers (the API returns `limit=0` 429s). If you have a paid Pro plan via
**Antigravity**, you can sign in with Google instead.

OAuth requires a Google OAuth desktop client. `mogen` reads the credentials
from a JSON file at runtime — they are deliberately **not** committed to this
repo. Create `oauth_client.json` with your `client_id` / `client_secret`:

```json
{
  "client_id": "1234567890-xxxxx.apps.googleusercontent.com",
  "client_secret": "GOCSPX-xxxxx"
}
```

Path resolution (first hit wins):

1. `$MOGEN_OAUTH_CLIENT` (full path to the JSON file)
2. `$MOGEN_CACHE_DIR/oauth_client.json`
3. `~/.mogen/oauth_client.json` — primary default, both Unix and Windows
   (e.g. `C:\Users\<you>\.mogen\oauth_client.json`)
4. `~/.cache/mogen/oauth_client.json` (legacy, kept for existing installs)
5. `%LOCALAPPDATA%\mogen\oauth_client.json` (legacy, Windows)

See `oauth_client.example.json` for a template. The Antigravity desktop client
that ships with the official Antigravity install is one valid source for
`client_id` / `client_secret`; you can also create your own desktop OAuth
client in Google Cloud Console with the redirect URI
`http://localhost:51121/oauth-callback`.

Then sign in:

```
mogen auth login            # browser-based Google sign-in
mogen auth status           # show email + project + token expiry
mogen auth logout           # delete the local token
mogen generate "a chair"    # uses OAuth automatically when GEMINI_API_KEY is unset
```

Credential precedence on `generate`/`modify`/`animate`/`repair`/`bench` is
`--api-key` flag > `GEMINI_API_KEY` env > stored OAuth token > error. Setting
the env var temporarily (or passing `--api-key`) shadows the OAuth token —
`mogen auth status --verbose` flags this when it happens. Tokens live at
`~/.mogen/google_auth.json` (`C:\Users\<you>\.mogen\google_auth.json` on
Windows) with mode `0600` on Unix. Older installs that wrote to
`~/.cache/mogen/` or `%LOCALAPPDATA%\mogen\` still work — those paths are
read as legacy fallbacks.

OAuth-mode requests go to `cloudcode-pa.googleapis.com/v1internal:generateContent`
on the user's own Cloud project (discovered via `loadCodeAssist`); the public
`generativelanguage.googleapis.com` API is not used in this mode.

**Caveat:** if you reuse Antigravity's OAuth client, those credentials are
extracted from a third-party desktop binary and Google can revoke them at any
time. Keep `GEMINI_API_KEY` available as a fallback, or register your own
desktop client. Image generation (`mogen textures`) defaults to API-key only
— pass `--allow-oauth-image` if you want to probe the Cloud Code Assist
surface for image generation (unverified there). The `cachedContents`
system-instruction cache is also disabled in OAuth mode (the surface doesn't
expose it); the system instruction is sent inline instead.

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
