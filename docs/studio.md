# MoGen Studio

A desktop editor for `.mog` scenes, built on the same compile pipeline as the CLI. It
adds a live 3D preview, an inspector, viewport gizmos, syntax highlighting, and
persistent settings.

Settings live in `~/.mogen/settings.json` (`C:\Users\<you>\.mogen\settings.json` on
Windows), shared with the CLI. Older installs' settings (e.g. `~/.config/mogen/settings.json`)
are read and migrated on first launch.

- [Launching](#launching)
- [The window at a glance](#the-window-at-a-glance)
- [Tabs and recent files](#tabs-and-recent-files)
- [The editor pane](#the-editor-pane)
- [The 3D viewer](#the-3d-viewer)
- [The inspector](#the-inspector)
- [Diagnostics](#diagnostics)
- [Building](#building)
- [LLM tools — generate, modify, animate, repair, textures](#llm-tools)
- [Themes and preview shaders](#themes-and-preview-shaders)
- [Settings](#settings)
- [Remote control](#remote-control)
- [Keyboard shortcuts](#keyboard-shortcuts)
- [File layout on disk](#file-layout-on-disk)

---

## Launching

```sh
cargo run --release -p mogen-studio          # from a source checkout

mogen-studio                                 # Linux release
open "/Applications/MoGen Studio.app"        # macOS .dmg
# Windows: Start menu after running the .msi installer.
```

The welcome dialog offers **Use Codex subscription** or an OpenAI API key (default
provider: OpenAI GPT-6 Astra). You can skip it; editing, viewing, and building need no
key. Keys are stored in the settings file and match the CLI's `OPENAI_API_KEY` etc.
Other providers (Gemini API key / Google OAuth, and the rest listed in
[the CLI reference](./cli.md#providers-and-credentials)) are in the provider dropdown.
Texture generation has its own image-provider setting.

**Codex subscription:** install a Codex CLI with `--ignore-user-config` support, run
`codex login`, then pick **OpenAI Codex (subscription)** under
**Edit → Preferences → LLM**. Set the binary path there if `codex` isn't on PATH. Both
model tiers default to `gpt-6-astra`. Requests use your plan limits and show zero API
cost in the spending meter.

---

## The window at a glance

| region | what it does |
|---|---|
| **menu bar** | File / Edit / View / Generate / Options / Help, with the tab strip below |
| **editor pane** | `.mog` source with highlighting, autocomplete, error squiggles, and a diagnostics footer |
| **3D viewer** | Live preview with translate/rotate/scale gizmos and click-to-select |
| **inspector** | Collapsible side panel: node attributes, materials, texture roster, per-file build options |

The editor and viewer are separated by a draggable splitter.

---

## Tabs and recent files

- Open tabs and the active tab are restored on next launch. Untitled buffers are not
  persisted — save them first.
- **File → Open Recent** lists the last 12 paths, newest first; missing files are dropped.
- New tabs are "Untitled" and ask for a path on first save.
- Studio opens and saves `.mogb` (the experimental MOGB container, see
  `mogen pack`/`unpack`) transparently; the editor always shows DSL source. Text `.mog`
  stays canonical.

---

## The editor pane

- **Syntax highlighting** — kinds, attributes, strings, numbers, comments, `$param`
  references. Works on source that doesn't parse yet.
- **Autocomplete** — kinds, attributes, and material names. <kbd>Tab</kbd> /
  <kbd>Enter</kbd> accepts, <kbd>Esc</kbd> cancels, <kbd>↑</kbd>/<kbd>↓</kbd> moves.
- **Indent on enter** — `{` indents; `}` de-indents.
- **Error squiggles** — at each diagnostic's span; hover for the message.
- **Find** — <kbd>Ctrl+F</kbd> / <kbd>⌘F</kbd>.
- **External-edit detection** — prompts to reload when the file changes on disk (or keep
  your unsaved copy).

---

## The 3D viewer

Renders the scene from memory after each successful build (no GLB written for preview).

- **Left-drag** orbits; **middle-drag** or <kbd>Shift</kbd>+left-drag pans; **wheel** zooms.
- <kbd>⌘0</kbd> / <kbd>Ctrl+0</kbd> frames the scene.
- **Left-click** selects the node under the cursor (highlighted in viewer and inspector);
  **Esc** clears. Clicks on generator output select the generator node.
- **Gizmos** — the selected node gets a translate / rotate / scale handle (mode in the
  View menu or on the gizmo). Dragging writes `pos` / `rot` / `scale` back into the
  source, leaving other formatting untouched.

---

## The inspector

- **Attribute fields** — numeric drags, vec3 spinners, colour pickers, dropdowns for
  enum attrs (`anchor`, `axis`, `alpha_mode`). Procedural generators show their
  parameters here; editing one rebuilds the generated geometry.
- **Material editor** — one section per material (base color, roughness, metallic,
  alpha mode, transmission, emissive, …), written back to the `material "…"` declaration.
- **Texture roster** — ✓ / ✗ per texture path, so missing files show before you build.
- **LOD scale slider** — edits the top-level `lod_scale (value=N)`; returning to `1.0`
  removes the directive. Imported geometry keeps its own file's setting; scale it with an
  enclosing group's `lod=` (see [LOD overrides](./dsl.md#per-node-lod-overrides)).
- **Per-file export options** — `include_animations`, `include_textures`,
  `merge_sibling_meshes`.

Inspector and gizmo edits appear in the editor text immediately, preserve comments and
whitespace, and are saved with the file.

---

## Diagnostics

Every build runs the CLI's validator (same format as `mogen check --json`). Diagnostics
appear as:

1. **Error squiggles** at the source span.
2. **Diagnostics footer** under the editor (hidden when empty).
3. **Tab badges** — red dot for errors; info-only tabs look clean.

---

## Building

**Build** (<kbd>⌘B</kbd> / <kbd>Ctrl+B</kbd>) compiles the tab to `<file>.glb` beside
the source and refreshes the viewer — the same pipeline as `mogen build`, plus the
optional per-file sibling-mesh merge. Validation errors abort without writing a GLB.

<kbd>F5</kbd> re-runs validation only.

---

## LLM tools

The Generate menu mirrors the LLM subcommands; each opens a dialog for the prompt and
options.

| menu item | CLI equivalent |
|---|---|
| **New from prompt…** (<kbd>⌘⇧N</kbd>) | [`mogen generate`](./cli.md#generate) |
| **Modify…** | [`mogen modify`](./cli.md#modify) |
| **Animate…** | [`mogen animate`](./cli.md#animate) |
| **Repair** | [`mogen repair`](./cli.md#repair) |
| **Generate textures…** | [`mogen textures`](./cli.md#textures) |

- Runs in the background with progress in the status line.
- Generate opens a new tab; Modify / Animate / Repair replace the current tab's contents.
- Seeds are embedded in the DSL header; the seed field is pre-filled from the file.
- Thinking level, model, temperature, and `max_repair_iters` come from
  **Options → Models**, with per-dialog overrides.
- Requires credentials for the selected provider (Preferences or its env var).

---

## Themes and preview shaders

**UI themes** (Options → Theme), stored by key; unknown keys fall back to Nord:

| key | label |
|---|---|
| `dark` | Dark |
| `light` | Light |
| `sunset` | Sunset (warm) |
| `nord` | Nord (cool) — *default* |
| `high-contrast` | High Contrast |

**Preview shaders** (View → Preview shader) affect the viewer only, not exports:

| key | label | shows |
|---|---|---|
| `standard` | Standard (PBR) | full PBR with textures — *default* |
| `toon` | Toon (cel-shaded) | hard-shaded NPR |
| `crt` | CRT (scanlines) | scanlines + bloom |
| `matcap` | Matcap (clay) | unlit sculpt-style |
| `wireframe` | Wireframe | edges only; useful after CSG or mesh merge |

---

## Settings

Plain JSON, safe to hand-edit (reloaded on next launch).

| key | meaning |
|---|---|
| `provider_slot` | Selected provider. Empty → OpenAI. |
| `codex_path` | Codex executable. Empty → `codex` on PATH. |
| `codex_model` / `codex_fast_model` | Codex models. Empty → `gpt-6-astra`. |
| `openai_api_key` | OpenAI key. Falls back to `OPENAI_API_KEY`. |
| `openai_model` | Empty → `gpt-6-astra`. |
| `openai_fast_model` | Prompt Enhancer / Ask model. Empty → `gpt-5-mini`. |
| `gemini_api_key` | Gemini key (Gemini API-key slot and image generation). |
| `gemini_model` | Empty → `gemini-pro-latest`. |
| `gemini_fast_model` | Prompt Enhancer model. Empty → `gemini-flash-latest`. |
| `gemini_temperature` | `null` → `0.3`. |
| `thinking_level` | `low` / `medium` / `high` / `xhigh`. Empty → `high`. |
| `max_repair_iters` | `null` → `2`. |
| `seed_override` | `null` → seed from DSL header, else random. |
| `theme` | UI theme key. |
| `preview_shader` | Viewer shader key. |
| `last_opened` | Last `.mog` opened. |
| `open_tabs` | Paths of titled tabs at last save. |
| `recent_files` | Recent paths, newest first, max 12. |
| `onboarded` | First-launch onboarding dismissed. |
| `remote_enabled` | Serve the remote-control dashboard. Default off. |
| `remote_port` | Dashboard port. `null`/`0` → `7878`. |
| `remote_allow_lan` | Bind `0.0.0.0` instead of loopback. Default off. |

Other provider keys (`anthropic_api_key`, `fireworks_api_key`, `zai_api_key`, …) use the
same file; see [the CLI reference](./cli.md#providers-and-credentials).

---

## Remote control

**Options → Remote** starts an embedded web dashboard (default
`http://127.0.0.1:7878/`) mirroring the session: tabs, active source, diagnostics, scene
stats, and a live orbiting preview. From the browser you can edit source (joins the undo
stack), save, recompile, and **Build GLB**.

- Off by default; the server starts as soon as it's enabled.
- Loopback only unless *Allow connections from other devices* is ticked. There is
  **no password** — only enable that on trusted networks.
- The preview renders only while a browser is watching and yields to thumbnail / video /
  publish renders.

---

## Keyboard shortcuts

`⌘` is `Ctrl` outside macOS.

| shortcut | action |
|---|---|
| <kbd>⌘N</kbd> | New untitled tab |
| <kbd>⌘⇧N</kbd> | New from prompt (AI generate) |
| <kbd>⌘O</kbd> | Open file… |
| <kbd>⌘⇧I</kbd> | Import… |
| <kbd>⌘S</kbd> | Save active tab |
| <kbd>⌘⇧S</kbd> | Save As… |
| <kbd>⌘B</kbd> | Build active tab to GLB |
| <kbd>F5</kbd> | Re-run the validator without building |
| <kbd>⌘W</kbd> | Close active tab |
| <kbd>⌘⇧T</kbd> | Reopen closed tab |
| <kbd>⌘0</kbd> | Frame the scene |
| <kbd>⌘F</kbd> | Find |
| <kbd>⌘⇧M</kbd> | Insert module reference… |
| <kbd>F1</kbd> | Documentation (Ctrl+click a keyword in the editor jumps to its docs section) |
| <kbd>⌘,</kbd> | Open Options |
| <kbd>⌘Q</kbd> | Quit (prompts to save dirty tabs) |
| <kbd>Tab</kbd> / <kbd>Enter</kbd> | Accept autocomplete |
| <kbd>Esc</kbd> | Dismiss autocomplete; clear selection |

Standard editing shortcuts (<kbd>⌘C</kbd>, <kbd>⌘X</kbd>, <kbd>⌘V</kbd>, <kbd>⌘A</kbd>,
<kbd>⌘Z</kbd>, <kbd>⌘⇧Z</kbd>) work in the editor.

---

## File layout on disk

```
my-project/
├── chair.mog              # source
├── chair.glb              # build output
└── textures/
    └── chair/             # mogen textures default output
        ├── wood_base_color.png
        ├── wood_normal.png
        ├── wood_metallic_roughness.png
        └── wood_occlusion.png
```

Texture paths are resolved relative to the `.mog`. The per-file `textures/<mog-stem>/`
folder keeps sibling files with shared material names from overwriting each other.

---

## See also

- [DSL reference](./dsl.md)
- [CLI reference](./cli.md)
- [Module catalog](./modules.md)
