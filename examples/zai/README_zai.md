# Z.ai (GLM) demo examples

These four prompts exercise the Z.ai chat surface end-to-end after the
seed/temperature wire fixes plus the GLM-5V-Turbo vision wiring landed.
Each example exists either as a committed `.mog` (re-runnable from the
DSL) or as a documented command for users with a working `ZAI_API_KEY`.

The Coder pass on Z.ai uses **`glm-5.1`** (text). When an image is
attached on the Studio's New from Prompt dialog, the worker silently
swaps the model to **`glm-5v-turbo`** for that single call. The
`--auto-refine` path on the CLI does the same swap before the Reviewer
call (see `crates/mogen/src/commands/generate.rs` and `modify.rs`).

## Generated and committed

These were produced live against `https://api.z.ai/api/paas/v4` on
2026-05-07.

### `zai_text.mog` — text-only Z.ai (`glm-5.1`)

```
target/release/mogen.exe generate --provider zai \
  --thinking low --max-repair-iters 1 \
  --dsl-out examples/zai/zai_text.mog --dry-run \
  "a small wooden stool with three legs"
```

Three-cylinder stool, single material, four-line scene block. Validates
the seed + temperature wire fixes (without them the request 400s before
the model ever sees the prompt).

### `zai_plan.mog` — Architect-Coder split (`--plan`)

```
target/release/mogen.exe generate --provider zai \
  --thinking low --max-repair-iters 1 --plan \
  --dsl-out examples/zai/zai_plan.mog --dry-run \
  "a moon-phase astrolabe with engraved rings"
```

Two-phase generate: planner emits a Markdown breakdown, Coder pass turns
it into a `solid`-wrapped `group` with three concentric rings, a moon
disc, a tilted ecliptic ring, and three `spin` clips. Confirms
`generate_plan` / `compose_coder_prompt` work against Z.ai's chat
surface.

## Documented but not committed (Z.ai connection drops)

Z.ai's `glm-5.1` endpoint occasionally drops the connection on long
generations under heavy system instructions
(`error sending request … An existing connection was forcibly closed by
the remote host. (os error 10054)`). When the service is healthy, these
two commands produce valid scenes — the wire format is verified by the
unit + mock-server tests, the runtime failure is upstream.

### `zai_refine.mog` / `.glb` — `--auto-refine 1` with vision Reviewer

```
target/release/mogen.exe generate --provider zai \
  --thinking low --max-repair-iters 1 --auto-refine 1 \
  --out examples/zai/zai_refine.glb \
  --dsl-out examples/zai/zai_refine.mog \
  "a brass-fittinged steam locomotive lantern"
```

The Coder pass uses `glm-5.1`; the Reviewer pass auto-swaps to
`glm-5v-turbo` because `Provider::Zai` now returns `true` from
`supports_images()`. **Status: blocked — Z.ai dropped both attempts on
2026-05-07.** Re-run later or in Studio (Refine button on a Z.ai tab).

### `zai_vision.mog` — image-to-3D from a reference PNG

```
# Studio-only (CLI generate has no --image flag yet):
# 1. Studio → File → New from Prompt
# 2. Click "Choose image…" and pick a PNG/JPG
# 3. Provider: Z.ai
# 4. Click Generate
# The worker forces glm-5v-turbo for this call.
```

**Status: blocked — `mogen generate` has no `--image` argument.** The
new vision path is exercised end-to-end by:

- the `zai_chat::tests::user_image_becomes_image_url_part` unit test
  (pins the wire shape: `content: [{type:"text"}, {type:"image_url", …}]`
  with a `data:<mime>;base64,...` URL);
- the `mock_server::zai_vision_uses_image_url_content` integration test
  (runs the full `LlmClient::generate` pipeline against a tiny_http
  Z.ai stub);
- the Studio worker's auto-swap in
  `crates/mogen-studio/src/app/util/llm.rs::run_llm` (when
  `provider == Zai && !cfg.user_images.is_empty()`).
