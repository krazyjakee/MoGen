# MoGen Studio UX Audit — 2026-05

Audit of the MoGen Studio desktop GUI (`mogen-studio` crate) covering five
surfaces: first-run / menus / file picker, code editor, LLM-driven flows
(generate / modify / textures), 3D viewport, and dialogs / settings / lifecycle.

Approach: read-only review of the source. No interactive testing, so timing,
animation, and focus-restoration claims that depend on runtime are flagged but
not confirmed.

Severity rubric:

- **P0** — user can lose work, data corruption, or hard dead-end.
- **P1** — friction that stalls or confuses a real workflow.
- **P2** — polish, consistency, microcopy.

Counts: 0 P0 · ~70 P1 · ~115 P2 across all surfaces.

---

## 1. Cross-cutting patterns

These show up everywhere and dominate the friction list. Fixing the pattern is
worth more than fixing each instance.

### 1.1 Disabled buttons without inline reasons

Roughly 25 buttons across menus, the LLM panel, the materials panel, the export
dialog, and the publish dialog disable themselves without explaining why. The
explanation usually exists, but only on hover. Examples:

- `app/ui_llm/main.rs:53` — Modify is disabled when no key, busy, or src empty;
  same disabled state, three different causes, one tooltip.
- `app/ui_llm/main.rs:228` — Generate Textures disables on missing path; reason
  shown in a separate banner the eye doesn't connect to the button.
- `app/ui_panels/materials.rs:549` — Generate / Regenerate disables silently.
- `app/file_picker.rs:513` — Confirm button disables with no inline message.
- `app/ui_menu.rs:528` — Publish disables; tooltip says "sign in via Community
  window" but you can't get to that window from here.

**Pattern fix:** when a button is disabled, append the reason to the label
itself or render a one-line hint immediately above/below it. Don't make users
hover to learn the gating condition — they often don't realize the button is
even interactive.

### 1.2 Empty / deselected states are too quiet

- Empty viewport on first launch: black canvas, no copy (`viewer.rs:535`).
- File picker, empty folder: weak grey "(no .mog files here)"
  (`file_picker.rs:582`).
- Open Recent, empty: weak grey "(no recent MOG files)" (`ui_menu.rs:106`).
- Selected panel, no selection: helpful hint, but consumes panel real estate
  permanently (`ui_panels/selected.rs:18`).
- Viewport context menu, no imports: a *disabled button* labelled "(no
  imports)" reads as broken (`viewport_menu.rs:247`).

**Pattern fix:** treat empty states as a teaching moment. Render at body size
(not `.weak()`), and include the next action: "Right-click in the viewport to
add a primitive, or File → New from Prompt to generate one."

### 1.3 Terminology drift

The same operation has multiple names in different parts of the UI:

- "Build GLB" (button) vs "Compile" (tooltip on the same button,
  `ui_menu.rs:155`).
- "Render" (`ui_menu.rs:75, 179, 194` — used in tooltips for thumbnail and
  video) vs "Generate" (used in the labels of those same items).
- "MoG file" (sometimes) vs "MOG file" (other times) vs ".mog" (in the picker)
  — `file_picker.rs:54` and elsewhere.
- "Modify current:" / "Animate current:" vs "Repair current:" / "Textures:"
  (LLM panel section headers — `ui_llm/main.rs`).

**Pattern fix:** pick one verb per pipeline stage and use it everywhere.
Recommended: **Build** = compile to GLB, **Generate** = LLM-driven content
creation, **Render** = thumbnail/video output. Then run a sweep replacing
inconsistent labels.

### 1.4 Prerequisite paths buried in error text

Several flows require setup elsewhere, and the only way to discover the
prerequisite is to try the broken thing first:

- No LLM key → click Generate → read banner → memorise menu path → quit dialog
  → open Preferences. There is no clickable link from the error to its fix
  (`app/ui_dialogs/ask.rs:90`, `app/ui_llm/main.rs:10`, `app/error_class.rs`).
- Publish requires being signed in to MoGHub from the Community window
  (`app/ui_menu.rs:528`); the disabled tooltip names the destination but does
  not open it.
- Save-As-Untitled-then-Export friction in `app/ui_dialogs/export.rs:38`: user
  must close the export dialog, save the file, reopen the dialog. No "Save and
  Export" button.

**Pattern fix:** make the next step a button. "Open Preferences", "Sign in",
"Save first" should be one click from the dialog that surfaces the
requirement.

### 1.5 Long operations without tangible progress

- Video render (`ui_menu.rs:688`): can take 30+ seconds; current UI is a modal
  that closes and a status bar that doesn't tick.
- Texture generation per material: a stage label changes, but per-material
  status, success/fail markers, and per-call cost are not surfaced
  (`ui_llm/progress.rs:104`).
- Update check (`app/update.rs`): "Cancel" button label doesn't change while
  fetching from GitHub; user can't tell if the network is slow or stuck.
- Shadow atlas reallocation (`viewer.rs:278`): GPU stall on next frame is
  invisible.

**Pattern fix:** every operation that can exceed ~1.5s needs a visible state
transition: spinner + caption + cancel. Per-item progress when N items
(materials, files, frames) are processed.

### 1.6 Cost is opaque

Texture generation runs through Gemini Image, billed per image, and a typical
scene has several materials. There is no pre-flight estimate. The session
meter (`ui_llm/session.rs:24`) shows running cost, but:

- Doesn't separate text calls from image calls.
- Doesn't warn at the long-context tier breakpoint.
- Per-iteration repair-loop cost is invisible — a 4-iter repair on Pro can
  silently double the bill.

**Pattern fix:** before any LLM image call, show "N materials × ~$0.04 = ~$X.
Continue?". Add an "estimated cost so far" line to the progress card.

### 1.7 Reproducibility hidden

`mogen` stamps a seed into the .mog header and the CLI exposes it. Studio does
not expose the seed in either the New Prompt dialog or the LLM panel. Users
can't A/B test prompts, can't lock down a result they liked, and can only
reproduce by editing the .mog header by hand.

**Fix:** add a seed field to the New Prompt dialog and to Preferences (with a
"randomise" button). Display the resolved seed on the progress card.

### 1.8 Platform-specific shortcuts hard-coded

`app/text_menu.rs:93–159` shows shortcuts as the literal string `"Ctrl+X"` even
on macOS. The right-click menu therefore lies on Mac. `ui_menu.rs:248`
correctly uses an OS-aware modifier elsewhere, so this is a localised bug.

**Fix:** route every shortcut label through `ctx.format_shortcut()` (or the
existing helper in `ui_menu.rs`).

### 1.9 `.small()` violations

CLAUDE.md forbids `.small()` on user-facing text except for icon-only buttons.
Concrete violations found:

- `autocomplete.rs:289` — completion tag uses `TextStyle::Small`. Tags are
  meaningful (function vs property vs keyword); shrinking them defeats them.
- `splash.rs:120` — stage label rendered at 13.0 px; barely legible at 1080p.
- `app/ui_panels/animation.rs:166` — `🗑` delete button is `.small()` even
  though it carries no other affordance.
- Several `small_button` calls in `app/ui_panels/overlay.rs` for primary
  actions (Frame, gizmo mode) — not text but visually cramped.

**Fix:** drop `.small()` from each; verify the layout still fits.

### 1.10 Silent fallthrough on missing prerequisites

- Open a saved tab whose file was deleted between sessions: tab opens with a
  broken compile, no warning at restore time (`app.rs:397`,
  `app/files.rs:43`).
- Multi-cursor pruning when source shrinks (`multi_caret.rs:38`): selections
  vanish without notice.
- Cmd+D wraparound when there are no more occurrences: silent.
- Select All / Cut / Paste from menu when no text field has focus: silent.
- Clipboard errors in the right-click Paste handler: silent.

**Fix:** every silent fallthrough should produce a one-line status-bar message.

---

## 2. P0 — none confirmed

The exploration agents flagged one P0 (animation clip-list desync after
recompile, `app/ui_panels/animation.rs:43`). Verified the code path:
`active_clips()` is stored by index and can map to the wrong clip if a
recompile reorders clips. The padding logic prevents OOB but not semantic
mis-mapping. Real risk, but rare in practice (clip reordering requires the
user to delete or insert a clip definition, which they will notice). **Treat
as P1.** Long-term fix: key `active_clips` by clip name, not index.

---

## 3. Top 12 quick wins (recommended first PR scope)

Concrete, low-risk, individually shippable. Roughly ranked by user impact per
hour of work.

1. **Tab dirty indicator.** Tabs show no asterisk or dot when the buffer has
   unsaved changes (`app/files.rs`, `ui_menu.rs:854`). One-line fix; saves
   real lost-work incidents.
2. **First-launch empty viewport copy.** Black canvas on first run is the
   single worst first impression. Add a centred message: "Open a .mog file or
   File → New from Prompt to generate one. Right-click the viewport to add
   primitives." (`viewer.rs:535`).
3. **Disabled-button reasons inline.** Rewrite the disabled state of Modify,
   Generate, Generate Textures, and Repair so the gating condition is part of
   the label, not the tooltip.
4. **OS-aware shortcuts in right-click menu.** Replace literal "Ctrl+X" with
   `ctx.format_shortcut(...)` (`app/text_menu.rs`).
5. **Seed field in New Prompt dialog.** One row, one TextEdit, one randomise
   button (`app/ui_dialogs/new_prompt.rs`). Unblocks reproducibility.
6. **Texture cost preflight.** Before kicking off the textures pipeline, show
   "N materials × ~$0.04 = ~$X. Continue?" (`app/ui_llm/main.rs:228`).
7. **Diagnostic line:col is clickable.** In the diagnostics panel
   (`ui_panels/diagnostics.rs:60`), make the location label scroll the editor
   and place the caret. Cuts time-to-fix per error roughly in half.
8. **Frame button promoted to viewport overlay toolbar.** Currently in a
   submenu (`ui_panels/overlay.rs`). Standard 3D-app convention; one
   reposition.
9. **Save-and-Export shortcut in export dialog.** When file is untitled, add
   a "Save…" button next to the warning instead of forcing a dialog round-trip
   (`app/ui_dialogs/export.rs:38`).
10. **Quit/Close dialog primary-button focus.** Default-focus "Save All" so
    Enter saves; today the user must reach for the mouse
    (`app/ui_dialogs/quit.rs`).
11. **Theme live preview.** Apply the theme on selection rather than on Save
    so users can compare without re-opening Preferences (`theme.rs`,
    `app/ui_dialogs/prefs.rs`).
12. **Tab close-button hit area.** The `×` is a `small_button`; missed clicks
    are common (`ui_menu.rs:940`). Use a normal-size button with an on-hover
    background.

---

## 4. Per-surface findings

For each surface, the most important issues. Severity in brackets, file:line
where useful. Items already covered in §1 are not repeated.

### 4.1 First run, splash, onboarding (`app.rs`, `splash.rs`,
`app/onboarding.rs`)

- **[P1]** Splash min dwell of 4 s (`app.rs:103`) is long when there are no
  tabs to restore; lower to 1.5–2 s.
- **[P1]** After Save in onboarding, the only confirmation is a status-bar
  line buried under the closing modal (`onboarding.rs:147`). User can't tell
  the key persisted. Show inline confirmation before close.
- **[P1]** `GEMINI_API_KEY` fallback is documented in onboarding but not in
  Preferences (`prefs.rs:345`). Both should mention the env var and which
  takes precedence.
- **[P1]** Crash-consent text rendered `.weak()` (`app/crash_consent.rs:62`).
  Privacy disclosure shouldn't be de-emphasised.
- **[P2]** Onboarding "Skip" path doesn't warn that LLM features will nag
  later.
- **[P2]** Onboarding success env-var box uses an off-palette blue
  (`onboarding.rs:96`).

### 4.2 Top menu and file picker (`app/ui_menu.rs`,
`app/file_picker.rs`, `app/viewport_menu.rs`)

- **[P1]** Filename field in Save As (`file_picker.rs:452`) doesn't say `.mog`
  is auto-appended. Users wonder if their file will be `my_scene` or
  `my_scene.mog`.
- **[P1]** "Close all" / "Close to right" silently skip dirty tabs
  (`ui_menu.rs:867`). User expects "all gone" and gets a partial result.
  Either confirm-and-save like Cmd+Q does, or show "Skipped 3 unsaved tabs"
  status.
- **[P1]** Add submenu (`viewport_menu.rs:174`) is a long flat list with no
  search; scroll-and-hunt for a specific primitive. Add a filter input.
- **[P2]** New Folder input in the picker (`file_picker.rs:407`) gives no
  cancel hint. Add "Esc to cancel."
- **[P2]** Path bar in picker (`file_picker.rs:392`) is editable but doesn't
  advertise paste-and-Enter affordance.
- **[P2]** "Up" button at root disables silently (`file_picker.rs:370`).
  Tooltip "Already at filesystem root."

### 4.3 Code editor (`app/ui_panels/editor.rs`,
`autocomplete.rs`, `app/find.rs`, `app/multi_caret.rs`,
`highlight.rs`)

- **[P1]** No current-line highlight in the gutter or the editor body
  (`ui_panels/editor.rs:100`). Hard to track the caret in a 200-line file.
- **[P1]** Diagnostics not click-to-jump (`ui_panels/diagnostics.rs:60`). See
  §3.7.
- **[P1]** Autocomplete popup is rendered in a separate `Area` after the
  editor; it does not scroll with the ScrollArea (`autocomplete.rs:165`). On a
  scrolled buffer the popup floats free of the line.
- **[P1]** Autocomplete suppression is keyed off source length
  (`autocomplete.rs:82`), not time. Type "x", delete, type "x" again — popup
  may not return. Switch to a 300 ms suppression window.
- **[P1]** `selected` index in autocomplete clamps but doesn't reset when the
  candidate set changes (`autocomplete.rs:115`); top match isn't preselected.
- **[P1]** Find bar Case-sensitivity toggle is "Aa" with no on/off visual
  state beyond a faint background fill (`find.rs:234`). Use a checkbox or
  pressed-state frame.
- **[P1]** No regex mode in find. Power users assume it's there.
- **[P1]** No keyboard-shortcut help dialog. Cmd+D, Cmd+L, Cmd+/, Alt+Up/Down,
  F3, Shift+F3 all exist (`app/line_ops.rs:8`); none are surfaced anywhere
  except the source.
- **[P1]** `app/text_menu.rs` hard-codes `Ctrl+X` etc. — see §1.8.
- **[P2]** Find substring match recomputes on every keystroke against the
  whole buffer with `to_lowercase()` per char (`find.rs:105`). Stutters on
  large files; cache the lowercased source.
- **[P2]** Indent uses literal `\t` (`indent.rs:85`); mixes with
  spaces-only files. Detect dominant style.
- **[P2]** No soft ruler (column-80 line). No word-wrap toggle.
- **[P2]** Multi-cursor exits silently when arrow keys collapse it
  (`multi_caret.rs:210`). Status hint: "Esc to clear multi-cursor."

### 4.4 LLM flows (`app/llm.rs`, `app/generate.rs`,
`app/ask.rs`, `app/ui_llm/*`, `app/ui_dialogs/new_prompt.rs`)

- **[P1]** No seed control. See §1.7 / §3.5.
- **[P1]** Texture generation runs without a cost preflight. See §3.6.
- **[P1]** "no Gemini API key — set GEMINI_API_KEY, paste a key in Edit →
  Preferences…, or switch to 'Gemini (Google OAuth)'"
  (`app/llm/credentials.rs:59`) packs three options into one sentence; users
  pick the wrong path or freeze. Split into stacked options with buttons.
- **[P1]** "Auto" image provider precedence is opaque
  (`app/ui_dialogs/prefs.rs:244`). Show the resolved choice live.
- **[P1]** Cancel hover text "may finish but its output is dropped"
  (`ui_llm/progress.rs:156`) is scary and ambiguous about billing. Clarify
  that any in-flight call still bills server-side.
- **[P1]** Save in Preferences stores the API key with no validation
  (`prefs.rs:163`). First failure surfaces only on Generate. Add a cheap
  "test key" call.
- **[P1]** Claude Code section says `claude /login` (`prefs.rs:306`). The
  command is `claude login` (no slash).
- **[P1]** Texture partial success (3/5 ok, 2 fail) renders as a red error
  banner (`app/llm/poll.rs:95`); user assumes the whole thing failed.
- **[P2]** "Thinking" budget UI talks in tokens of "hidden reasoning" without
  mapping to latency or quality (`app/ui_llm/thinking.rs:18`).
- **[P2]** Repair-loop dots animate but don't say what changed between
  iterations (`ui_llm/progress.rs:298`). Consider "iter 2: fixed 1 error,
  introduced 2."
- **[P2]** Microcopy lowercase / abrupt: "type a question first", "enter a
  prompt first", "thumbnail: nothing to render" (`app/ask.rs:94`,
  `app/llm.rs:30`, `app/generate.rs:112`). Sentence case + actionable phrasing.
- **[P2]** Provider label "Google (gemini-cli)" exposes the OAuth client name
  (`oauth_ui.rs:104`). Just say "Google (OAuth)".

### 4.5 3D viewport (`viewer.rs`, `gizmo.rs`,
`pick.rs`, `app/ui_panels/overlay.rs`, `app/ui_panels/selected.rs`,
`app/ui_panels/materials.rs`, `app/ui_panels/animation.rs`)

- **[P1]** Empty viewport on first run — see §3.2.
- **[P1]** Active-clip state stored by index, can shift on recompile if clip
  ordering changes (`ui_panels/animation.rs:43`). Key by name.
- **[P1]** Gizmo mode buttons labelled "T / R / S" with hotkey hints "(W)",
  "(E)", "(R)" (`ui_panels/overlay.rs:44`). Either fix labels to W/E/R or
  rebind hotkeys to T/R/S. Currently the button text and the hotkey hint
  contradict each other.
- **[P1]** Gizmo silently disappears on non-editable nodes — imports,
  CSG output, replicator members (`gizmo.rs` + `viewer/state.rs:592`). User
  thinks the gizmo is broken. Show a dim "read-only" gizmo or an inline
  reason.
- **[P1]** Imported / relative-placed nodes have no visual mark in the 3D
  view (`viewer/state.rs:109`). Selecting one and finding no gizmo is
  confusing. Outline imports in a distinct colour.
- **[P1]** "Imported via `use` — wrap in a group" warning in
  `selected.rs:102` lacks a one-click "wrap it for me" button.
- **[P1]** Multi-select indicator: header says "{n} nodes selected — editing
  primary" (`selected.rs:27`); the *primary* node is not visually
  distinguished in the viewport. Use a different selection colour for the
  primary.
- **[P1]** Inspector edits don't jump the editor caret to the modified node's
  span (`selected.rs:163`); viewport clicks do. Asymmetric.
- **[P1]** Snap step values (translate 0.25, rotate 15°, scale 25%) are
  invisible (`viewer/state.rs:200`). Expose in the overlay.
- **[P1]** Speed slider runs from −2× to +4× with no zero indicator
  (`ui_panels/animation.rs:107`). Accidentally dragging through zero plays
  backwards with no explanation.
- **[P1]** Animated shaders (water, etc.) keep painting while playback is
  paused (`viewer.rs:805`); confusing while editing materials.
- **[P1]** Generate / Regenerate texture button in materials panel disables
  silently with no inline reason (`ui_panels/materials.rs:549`).
- **[P1]** Missing texture file shows a red cross with no reason
  (`ui_panels/materials.rs:592`). On hover, surface "File not found:
  {path}".
- **[P1]** Cinema mode forces playback on (`viewer.rs:448`) without asking.
- **[P1]** Light gizmos don't scale with zoom (`viewer/lights.rs:52`).
- **[P2]** No FPS counter; no current-frame counter; no clip-loop indicator.
- **[P2]** Grid lacks unit labels (`viewer.rs:894`).
- **[P2]** Pick latency unbounded on heavy scenes (`pick.rs:31` is
  brute-force per-triangle).
- **[P2]** Drill-down (Figma-style) on repeated click is undiscoverable
  (`viewer/state.rs:959`); add a small "click again to drill" hint after the
  first click.
- **[P2]** Material panel thumbnails are 64 px squares with no click-to-zoom.

### 4.6 Dialogs, settings, lifecycle (`settings.rs`, `theme.rs`,
`app/ui_dialogs/*`, `app/files.rs`, `app/watcher.rs`, `app/update.rs`,
`app/error_class.rs`, `app/moghub.rs`)

- **[P1]** Tab dirty indicator missing — see §3.1.
- **[P1]** "Close all" / "Close to right" skip dirty tabs silently — see §4.2.
- **[P1]** Update dialog has no "Skip this version" / "Remind me later"
  (`app/ui_dialogs/update.rs:21`); same nudge every session.
- **[P1]** Untitled-file export round-trip — see §3.9.
- **[P1]** Publish form allows empty title and fails server-side
  (`community/publish.rs:118`). Disable Publish until title is set; red-mark
  the field.
- **[P1]** GitHub OAuth in MoGHub sign-in is silent if the browser fails to
  open (`community/auth.rs:11`). Log the URL and show a copy button.
- **[P1]** Community discover spins forever with no timeout when offline
  (`community/mod.rs:46`).
- **[P1]** Conflict modal X close button defaults to "Keep mine" on
  Modified-on-disk (`app/ui_dialogs/external.rs:14`); ambiguous to users who
  read X as "back".
- **[P1]** Conflict modal puts "Close tab" as the rightmost (default-eye)
  button (`external.rs:98`); destructive action in the primary slot.
- **[P1]** API-key field is masked, so accidental whitespace pasted with the
  key is invisible (`prefs.rs:343`). Trim on save and tell the user.
- **[P1]** First-launch onboarding hands users into an empty editor with no
  guided next step. After crash-consent, an LLM-setup wizard would help.
- **[P2]** Settings save provides no toast confirmation
  (`settings.rs:40`).
- **[P2]** No OS-theme detection on first launch (`theme.rs`).
- **[P2]** Quit-confirmation tab name is double-quoted: `""scene.mog" has
  unsaved changes` (`quit.rs:113`).
- **[P2]** About dialog links have no underline / hover effect
  (`about.rs:15`).
- **[P2]** Watcher pauses while a modal is open (`watcher.rs:25`); external
  changes are missed. Queue them and prompt after the modal closes.
- **[P2]** Many dialogs are non-resizable; tall content can clip on small
  screens.
- **[P2]** Generic button labels ("OK", "Cancel") on context-laden modals
  (e.g. file conflict). Prefer action verbs: "Keep my edits" / "Reload from
  disk".

---

## 5. Suggested execution order

If this lands as a stream of small PRs, an order that compounds well:

1. **Foundation (1 day):** tab dirty indicator, OS-aware shortcuts, drop the
   `.small()` violations, stop hard-coding `"Ctrl+…"` in the right-click menu.
2. **Empty states (1 day):** first-launch viewport copy, file-picker empty
   folder, Open Recent empty, viewport context-menu "no imports".
3. **Disabled-button rewrite (1 day):** sweep across LLM panel, materials
   panel, file picker, export dialog. One shared helper for "disabled with
   reason inline".
4. **Editor wins (2 days):** click-to-jump diagnostics, current-line
   highlight, autocomplete inside ScrollArea, time-based suppression, find-bar
   regex toggle and case-sensitivity affordance, keyboard-shortcuts help
   dialog.
5. **LLM flow wins (2 days):** seed field, texture cost preflight, OAuth
   sign-in button on the credentials banner, "test key" on Save, fix
   `claude /login` typo, partial-success texture banner, OS-correct
   provider labels.
6. **Viewport wins (2 days):** Frame button to toolbar, gizmo button-vs-hotkey
   label fix, dimmed read-only gizmo for non-editable nodes, zero-marker on
   speed slider, key clip state by name not index, snap settings exposed.
7. **Dialogs and lifecycle (1 day):** Skip-this-version on update,
   Save-and-Export on untitled, Publish title validation, conflict modal
   button reorder, theme live preview.

Total: roughly two engineer-weeks for the items above, which would clear
~70 % of the P1 list.

---

## 6. What this audit did not cover

- Runtime testing of focus order, screen-reader behaviour, high-DPI scaling.
- Cross-platform input quirks (Wayland, macOS trackpad gestures, Windows
  IME).
- Performance on large scenes — measured FPS, memory, GPU stalls.
- Localisation. All copy is English-only and a few strings concatenate
  poorly when translated (e.g. provider labels embedded mid-sentence).
- The CLI (`mogen` binary) — out of scope; this is the Studio audit.
