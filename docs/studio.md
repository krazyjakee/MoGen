# MoGen Studio

A desktop editor for `.mog` scenes. Studio is a thin GUI on top of the
same compile pipeline the CLI uses — every feature you can run from
`mogen <subcommand>` is also available as a button or menu item. It adds
a live 3D preview, a span-aware inspector, viewport gizmos, syntax
highlighting, and a settings store so per-project knobs stick across
sessions.

Window title is **MoGen Studio**. Settings live at
`~/.config/mogen/settings.json` on Linux (`%APPDATA%\mogen\settings.json`
on Windows; `~/Library/Application Support/mogen/settings.json` on macOS).

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
- [Keyboard shortcuts](#keyboard-shortcuts)
- [File layout on disk](#file-layout-on-disk)

---

## Launching

```sh
cargo run --release -p mogen-studio          # from a source checkout

# Or from a release artifact:
mogen-studio                                 # Linux tarball
open "/Applications/MoGen Studio.app"        # macOS .dmg
# On Windows: launch from the Start menu after running the .msi installer.
```

On first launch, Studio shows an onboarding dialog asking for a Gemini
API key. You can skip it — every non-LLM feature still works without one
— or paste a key in. The key is stored in the settings file and is the
same value `GEMINI_API_KEY` would supply to the CLI.

---

## The window at a glance

Studio's main view is a four-region layout:

| region | what it does |
|---|---|
| **menu bar** | File / Edit / View / Generate / Options / Help, plus the active-tab strip just below |
| **editor pane** | `.mog` source with live syntax highlighting, autocomplete, error squiggles, and a per-file diagnostics footer |
| **3D viewer** | Live preview of the most recently built `.mog`, with translate/rotate/scale gizmos and click-to-select picking |
| **inspector** | Selected-node attribute editor, material editor, texture roster, and per-file build options |

The editor and viewer are separated by a draggable splitter; the
inspector is a collapsible side panel.

---

## Tabs and recent files

Multiple `.mog` files can be open simultaneously, one per tab.

- The tab strip remembers order across sessions — closing Studio with
  three tabs open reopens all three on the next launch, with the last
  active tab focused.
- Untitled buffers (no path yet) are not persisted across restarts, but
  they survive accidental re-runs of Studio because every change is held
  in memory.
- File → Open Recent shows the last 12 opened paths, newest first. Items
  are removed automatically if the file no longer exists at that path.
- Save / Save As use a native file dialog. New tabs default to "Untitled"
  and prompt for a path on first save.

---

## The editor pane

The editor is a custom egui-based text view tuned for `.mog`. It does
NOT use a pest-driven highlighter on every keystroke — instead, a loose
tokeniser in `highlight.rs` colours kinds, strings, numbers, comments,
and `$param` references, so mid-edit source still highlights even when
the parser would reject it.

Features:

- **Syntax highlighting** — kinds, attributes, strings, numbers, comments, `$param` references.
- **Autocomplete** — typing a kind, attribute, or material name pops a fuzzy list. <kbd>Tab</kbd> / <kbd>Enter</kbd> accepts; <kbd>Esc</kbd> cancels; <kbd>↑</kbd>/<kbd>↓</kbd> moves the selection.
- **Indent on enter** — opening a `{` block bumps the indent; closing brace de-indents in place.
- **Error squiggles** — every diagnostic from the AST or graph validator gets an underline at the exact span the validator emitted. Hover for the message.
- **Find** — <kbd>Ctrl+F</kbd> / <kbd>⌘F</kbd> opens a find bar.
- **External-edit detection** — if the file changes on disk while a tab is open, Studio prompts to reload (or keep your in-memory copy if you have unsaved changes).

---

## The 3D viewer

A live preview built on `eframe`'s wgpu backend. Loaded directly from the
in-memory `SceneGraph` after each successful build — there's no
intermediate GLB on disk for the preview.

Camera:

- **Left-drag** orbits the focus point.
- **Middle-drag** (or <kbd>Shift</kbd>+left-drag) pans.
- **Mouse wheel** zooms.
- <kbd>⌘0</kbd> / <kbd>Ctrl+0</kbd> frames the whole scene.

Selection:

- **Left-click** picks the node under the cursor (Möller–Trumbore ray cast against the rendered meshes). The selected node highlights both in the viewer and in the inspector.
- **Esc** clears the selection.

Gizmos:

- When a node is selected, a translate / rotate / scale gizmo handle floats over its origin in world space. The mode toggles in the View menu (or the gizmo widget itself).
- Dragging a handle modifies `pos` / `rot` / `scale` on the selected node and **writes the change back into the source `.mog`** as a span-preserving edit. Other formatting in the file is left alone.

---

## The inspector

The inspector binds the currently selected node to its attributes:

- **Per-attribute fields** — drag-numeric inputs, vec3 spinners, colour pickers for material colours, dropdowns for enum-like attrs (`anchor`, `axis`, `alpha_mode`).
- **Material editor** — every authored material gets a collapsing section. Edit base color, roughness, metallic, alpha mode, transmission, emissive, etc.; changes flow back to the source `material "…"` declaration via the same span-preserving edit machinery.
- **Texture roster** — each material's texture slots show ✓ / ✗ markers based on whether the referenced PNG actually exists at the resolved path. Missing textures are visible *before* you try to build.
- **LOD scale slider** — edits the top-level `lod_scale (value=N)` directive in place. Drag down to iterate quickly on big scenes; drag back to `1.0` and Studio removes the directive entirely so saved files stay clean.
- **Per-file export options** — `include_animations`, `include_textures`, `merge_sibling_meshes` (sticky per file).

Edits made in the inspector and via gizmos are persisted into the
source file the next time you save. They are reflected in the editor
text immediately, and they preserve every comment, blank line, and
whitespace style elsewhere in the source.

---

## Diagnostics

Every build runs the same dual validator the CLI uses (AST-level + graph-level). The diagnostics show up in three places:

1. **Error squiggles** in the editor at the exact source span.
2. **Diagnostics footer** under the editor — collapsible; auto-hides when there are no errors or warnings.
3. **Tab badges** — a tab with errors gets a red dot; a tab with only info-level diagnostics looks clean.

`mogen check --json` and the studio share the same diagnostic format —
the studio is a viewer for that JSON, dressed up as a UI.

---

## Building

The **Build** button (or <kbd>⌘B</kbd> / <kbd>Ctrl+B</kbd>) compiles the
current tab to GLB and refreshes the 3D viewer. The output GLB lands at
`<file>.glb` next to the source by default.

The build pipeline is the same one `mogen build` runs:

1. Parse → AST → AST validation
2. Lower → SceneGraph
3. Graph validation
4. Optional sibling-mesh merge (per-file `merge_sibling_meshes` setting)
5. GLB export

Validation errors abort the build and surface in the diagnostics footer
without writing a GLB.

<kbd>F5</kbd> re-runs the validator only, without re-emitting a GLB.
Useful when you've edited a `.mog` from outside Studio and want a quick
diagnostic refresh.

---

## LLM tools

The Generate menu mirrors the LLM-driven CLI subcommands. Each opens a
small modal that collects a prompt and the relevant flags.

| menu item | CLI equivalent |
|---|---|
| **New from prompt…** (<kbd>⌘⇧N</kbd>) | [`mogen generate`](./cli.md#generate) |
| **Modify…** | [`mogen modify`](./cli.md#modify) |
| **Animate…** | [`mogen animate`](./cli.md#animate) |
| **Repair** | [`mogen repair`](./cli.md#repair) |
| **Generate textures…** | [`mogen textures`](./cli.md#textures) |

Behaviour matches the CLI:

- The repair loop runs in the background; progress is shown in the
  status line.
- The generated `.mog` opens in a new tab (Generate) or replaces the
  current tab's contents (Modify / Animate / Repair).
- Seeds are embedded in the DSL header so the same prompt + same seed
  re-emits the same scene. The seed field in each modal is pre-filled
  with the input file's existing seed when present.
- Thinking level, model, temperature, and `max_repair_iters` come from
  Options → Models. Per-modal overrides live next to the prompt field
  for one-off tuning.

If `gemini_api_key` is empty in the settings, every LLM action prompts
for a key first (the same onboarding dialog as on first launch).

---

## Themes and preview shaders

**UI themes** (Options → Theme):

| key | label | use |
|---|---|---|
| `dark` | Dark | classic dark editor |
| `light` | Light | bright office mode |
| `sunset` | Sunset (warm) | warm-toned |
| `nord` | Nord (cool) | cool-toned blue/grey — *default* |
| `high-contrast` | High Contrast | accessibility / projector |

The theme name is stored in `settings.json` as a lowercase label so new
variants can land without breaking older settings files. Empty / unknown
labels fall back to Nord.

**Preview shaders** (View → Preview shader) control the 3D viewer only —
they don't affect the exported GLB.

| key | label | what it shows |
|---|---|---|
| `standard` | Standard (PBR) | full PBR with embedded textures — *default* |
| `toon` | Toon (cel-shaded) | hard-shaded NPR look |
| `crt` | CRT (scanlines) | post-process scanlines + bloom |
| `matcap` | Matcap (clay) | unlit material capture for sculpt-style review |
| `wireframe` | Wireframe | edges only, ignores materials |

Wireframe is useful for inspecting topology after CSG or sibling-mesh
merge. CRT is for vibes.

---

## Settings

The settings file is a JSON document stored at the OS-appropriate config
path. It's safe to edit by hand — Studio reloads on next launch.

| key | meaning |
|---|---|
| `gemini_api_key` | API key used by every LLM action. |
| `gemini_model` | Heavy model id. Empty → `gemini-pro-latest`. |
| `gemini_fast_model` | Fast model id used for low-stakes rewrites (Prompt Enhancer). Empty → `gemini-flash-latest`. |
| `gemini_temperature` | Sampling temperature. `null` → library default (`0.3`). |
| `thinking_level` | `low` / `medium` / `high` / `xhigh`. Empty → library default (`high`). |
| `max_repair_iters` | LLM repair budget. `null` → library default (`2`). |
| `seed_override` | Optional deterministic seed. `null` → derive from DSL header or random per call. |
| `theme` | UI theme key (see above). |
| `preview_shader` | Viewer shader key (see above). |
| `last_opened` | Absolute path of the last `.mog` opened. |
| `open_tabs` | Absolute paths of every titled tab open at last persist time. |
| `recent_files` | Most-recently-opened paths, newest first. Capped at 12. |
| `onboarded` | Set once the first-launch onboarding has been dismissed. |
| `remote_enabled` | Serve the remote-control web dashboard. `null`/`false` → off. |
| `remote_port` | TCP port for the dashboard. `null`/`0` → `7878`. |
| `remote_allow_lan` | Bind `0.0.0.0` so other devices on the network can connect. `null`/`false` → loopback only. |

Untitled buffers are deliberately not persisted — there's nothing to
key off — so a fresh Studio launch with only-untitled tabs comes up
empty. Save first if you want them back.

---

## Remote control

Options › **Remote** can start an embedded web server that mirrors the
live session in any browser: the open tabs, the active source, compile
diagnostics, scene stats, and a slowly orbiting live preview of the
compiled model. The dashboard can push source edits back, save, force a
recompile, and kick off a **Build GLB** — every action lands in the
desktop session exactly as if it were performed there (remote edits even
join the in-app undo stack).

- Off by default. Enable it with the checkbox; the server starts
  immediately and the dialog shows the URL (default
  `http://127.0.0.1:7878/`).
- By default only this machine can connect. Ticking *Allow connections
  from other devices* rebinds to `0.0.0.0` so a phone or tablet on the
  same network can drive Studio — there is **no password**, so only do
  this on networks you trust.
- The preview renders on the same GL pipeline as thumbnails, only while
  a browser is actually watching, and always yields to real capture
  work (thumbnail / video / publish renders).

---

## Keyboard shortcuts

`COMMAND` is `⌘` on macOS and `Ctrl` elsewhere. All shortcuts work
globally; egui consumes them before the menu or editor see the key.

| shortcut | action |
|---|---|
| <kbd>⌘N</kbd> | New untitled tab |
| <kbd>⌘⇧N</kbd> | New from prompt (Gemini generate) |
| <kbd>⌘O</kbd> | Open file… |
| <kbd>⌘S</kbd> | Save active tab |
| <kbd>⌘⇧S</kbd> | Save As… |
| <kbd>⌘B</kbd> | Build active tab to GLB |
| <kbd>F5</kbd> | Re-run the validator without re-emitting the GLB |
| <kbd>⌘W</kbd> | Close active tab |
| <kbd>⌘0</kbd> | Frame the scene in the 3D viewer |
| <kbd>⌘,</kbd> | Open Options |
| <kbd>⌘Q</kbd> | Quit (with prompt-to-save for dirty tabs) |
| <kbd>Tab</kbd> / <kbd>Enter</kbd> | Accept autocomplete suggestion |
| <kbd>Esc</kbd> | Dismiss autocomplete; clear viewer selection |

Standard editing shortcuts (<kbd>⌘C</kbd>, <kbd>⌘X</kbd>, <kbd>⌘V</kbd>,
<kbd>⌘A</kbd>, <kbd>⌘Z</kbd>, <kbd>⌘⇧Z</kbd>) work in the editor pane.

---

## File layout on disk

For a given project directory, Studio expects (and creates) the
following layout:

```
my-project/
├── chair.mog              # source
├── chair.glb              # build output (next to the .mog by default)
└── textures/
    └── chair/             # mogen textures --textures-dir default
        ├── wood_base_color.png
        ├── wood_normal.png
        ├── wood_metallic_roughness.png
        └── wood_occlusion.png
```

Texture paths in `material "…"` declarations are resolved relative to
the `.mog` file. The `textures/<mog-stem>/` subdirectory is the default
output of `mogen textures` so sibling `.mog`s with shared material
names don't clobber each other.

---

## See also

- [DSL reference](./dsl.md) — the language Studio edits.
- [CLI reference](./cli.md) — the commands Studio wraps.
- [Module catalog](./modules.md) — reusable parametric modules shipped in the repo.
