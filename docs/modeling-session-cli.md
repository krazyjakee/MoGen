# Durable modeling sessions: CLI, MCP and Studio

`mogen session` uses the same `refine_session` / `resume_session` runner as
Studio. It generates or loads source, validates/builds it, captures five matched
views, requests a typed review, and runs scoped Inspect/Measure/Apply/Render
corrections. Invalid generated source enters that same tool dispatcher for
build repair. There is no custom Rust harness required.

```sh
# Linux headless software rendering; install libegl1, libegl-mesa0,
# libgl1-mesa-dri and their dependencies first.
EGL_PLATFORM=surfaceless LIBGL_ALWAYS_SOFTWARE=1 mogen session \
  --prompt 'An original red two-seat sports coupe, sculpted body, open wheel arches, low fastback cabin, detailed alloy wheels; metres, +Y up, front -Z' \
  --provider codex --model gpt-6-astra \
  --calls 24 --iterations 4 --seconds 2400 --output-tokens 24000 \
  --out-dir output/rosso-s1 --glb

# Persistent detailed brief (JSON ModelingBrief; see the live retry artifact).
mogen session --brief brief.json --provider codex --model gpt-6-astra \
  --out-dir output/rosso-detailed --glb

# Modify/refine an existing asset in a new isolated output project.
mogen session --input chair.mog --prompt 'Thicken the arms' \
  --lock seat --selected-part arm --out-dir output/chair-refined

# Resume the saved limits, brief, references, selection, locks and model.
EGL_PLATFORM=surfaceless LIBGL_ALWAYS_SOFTWARE=1 mogen session \
  --resume --provider codex --out-dir output/rosso-s1 --glb
mogen session --inspect --out-dir output/rosso-s1
```

Repeat `--reference image.png` and `--lock part_name` as needed. Reference images
are embedded in the sidecar, along with their digests. `--brief` accepts the
existing `ModelingBrief` JSON format with style, dimensions, intended use,
required details and constraints. All existing provider/credential/billing
routes remain available through `--provider`; keys are never saved in projects.
`--seed`, `--temperature` and `--thinking low|medium|high|xhigh` are optional;
resume restores the original sampling settings. An input asset's existing
sidecar supplies its persistent brief, references and locks. `--prompt` appends
a correction to that brief; an explicit `--brief` replaces it. Added locks are
combined with the saved locks.
`--spend-usd` stops if pricing is unknown. Subscription usage is reported as
unknown cost, not zero dollars. The selected model must accept images for
Refined mode; capability checks and a real GL probe run before generation.

`--draft` generates/builds a new asset without visual review or GL; it cannot
be combined with `--input`. Ordinary `generate`,
`modify` and their legacy `--auto-refine` flag retain their previous behavior.
Use `session` for durable review/tool recovery; that compatibility choice avoids
silently changing existing scripts' output paths and refinement behavior.
macOS/Windows use the existing hidden-window GL renderer and need an available
graphical session. Linux needs working EGL/OpenGL 3.3; a missing driver yields
`render_unavailable` before an avoidable model call. Set the EGL vendor/driver
paths if your Mesa installation is not in the system library directories.

## Outputs and stops

Stdout is one JSON report; progress goes to stderr every two seconds while a
call is pending, plus events at durable checkpoints. Ctrl-C requests
cancellation through the shared control. It does not claim provider billing
was reversed. The report includes absolute `final_mog`, `selected_mog`,
`latest_mog`, `sidecar`, `report` paths, selected/latest revisions, stop reason,
usage and elapsed time. `final_mog` is null if that file does not match the
selected snapshot (for example after a crash before final output was written).

The directory contains `final.mog`, optional `final.glb`,
`final.mog.modeling.json`, `report.json`, source candidates, and matched PNG
views with camera metadata in the report. Each `revision-N/` contains a complete
source/dependency snapshot recoverable even after external dependency edits.
`generation-response.txt` retains the initial raw response. Candidates explicitly
say whether they were reviewed and selected; receiving source is not acceptance.

| Status | Meaning |
|---|---|
| `completed` | Draft finished, reviewer completed, or runner stopped without a useful edit; read `stop_reason`. This is not a quality guarantee. |
| `budget_exhausted` | Call, iteration, time or spend gate stopped new work. Saved work remains. |
| `canceled` | Cancellation prevented further source mutation; late received responses can remain saved. |
| `review_format_failed` | Bounded format repair failed or could not preserve the original judgments. Raw responses remain in the journal. |
| `render_unavailable` | Headless/image capability failed; install drivers or choose a vision model/Draft. |
| `stopped` | Other provider, validation, stale-state or protocol error; see the explicit reason. |

MCP exposes `session` with the same parameters (including `resume`, `inspect`,
brief/reference paths, limits, selection and locks). It returns the CLI report
as `structuredContent` and retains error text. Model calls still run through
MoGen's existing credentials; MCP does not receive shell/filesystem tools from
the modeling protocol.

## Receipt journal and resume contract

Sidecar v2 loads v1 projects; old snapshots remain available, but a v1 session
has no transcript to resume. Save is atomic (temporary file, flush/sync, rename).
An incomplete temporary write leaves the prior sidecar intact; a corrupt final
sidecar fails explicitly. Candidate and capture revisions are checked on load.

Completed provider responses are saved **before parsing** with request/source
identity, phase, model/provider, usage and interpretation outcomes. Successful
Apply and each matched capture are saved immediately. Resume reconstructs the
private workspace from the initial snapshot and saved transcript, reusing exact
responses and captures before requesting the first incomplete operation. It may
repeat deterministic local compilation/validation, but does not resend completed
model calls or apply an edit twice to the current document. Charged usage and
elapsed budget are restored. Stale source/dependencies and changed brief,
model/provider, selection or locks reject resume; recover a snapshot or begin a
new session instead. Restart is not a fresh budget.

The active journal stops before a new call after 256 responses or 32 MiB of
retained response text. Responses over 1 MiB are saved for inspection but never
interpreted. Starting another session retains a bounded archive of previous
responses (oldest archived entries are evicted). Image/candidate retention follows
the existing project snapshots. A process killed **during an in-flight call**
may have no response to reuse; that call can be billed and a retry can be billed
again. Before dispatch, the sidecar reserves one call with unknown cost; if the
process dies before a receipt, resume conservatively charges that uncertain slot
against the call limit. A completed receipt replaces the reservation with actual
usage. Exactly-once remote billing is not promised.

Studio's Modeling session panel shows stage, saved responses, raw-response copy,
reviewed/unreviewed candidates and **Resume saved refinement**. Its existing
cancellation and candidate comparison remain available.

## Response contracts

Generation receives DSL guidance plus one DSL output contract. Tool phases use
that guidance with exactly one JSON operation contract, including experimental
guidance. They do not append JSON instructions to a DSL-only prompt. A complete
bare or whole fenced `mog` document can be staged as full-source Apply, bound
to the revision that requested it. Mixed prose, invalid DSL, stale dependencies,
focused-scope violations and indirect locked changes cannot execute atomically.

Review schema v1 has exactly `findings: string`, `complete: boolean`,
`improved: boolean`, `correction: string`. Whole JSON fences and arrays containing
only findings strings are losslessly normalized (newline join). No missing
booleans are defaulted. One format-only retry is allowed; its findings/judgments
must match the original evidence. An unparseable/truncated original cannot pass
that comparison and stays recoverable rather than becoming a new judgment.
Explicitly supported OpenAI models and Gemini transport the same native JSON
schema; other providers use the typed text example and identical validation.
See [OpenAI structured output](https://openai.com/index/introducing-structured-outputs-in-the-api/)
and [Gemini structured output](https://ai.google.dev/gemini-api/docs/structured-output).

## Deterministic and live validation

```sh
cargo test -p mogen-llm --lib
cargo test -p mogen-geom -p mogen-dsl -p mogen-validate --lib
cargo test -p mogen --tests
cargo check -p mogen-studio
# Existing #118 evaluator, now including the sports-body fixture:
EGL_PLATFORM=surfaceless LIBGL_ALWAYS_SOFTWARE=1 \
  cargo run -p mogen --example quality_eval -- --out output/quality-fixtures
```

`--script responses.json` supplies a JSON array of response strings as a fake
provider; it still uses the same call gate and runner. The library's fake
renderer tests cover crash injection and resume with no GL or credentials. The
CLI Draft integration test checks actual command parsing, sidecars and output
paths without GL. Live execution is opt-in by omitting `--script`; record the
brief, provider/model, declared limits, before/after matched views, usage,
latency, stop reason and every manual intervention. Judge aesthetic quality
separately from build/runner success.
