# Generate and Refine

Studio's ordinary **New from Prompt** and **Modify** workflows offer two modes.
**Draft** generates and repairs a candidate. **Refined** then inspects the result
from neutral front, side, back and three-quarter views plus a presentation view
with authored materials, and asks for targeted corrections. Draft remains the
default until reference-based evaluation justifies changing it.

The Modeling session panel stores the original target, reference images,
dimensions and units, intended use, required details, corrections and approved
constraints. Reference bytes travel through planning, coding, repair and review;
current renders are labeled separately. Image-only planning is supported on
vision-capable providers. An unsupported provider or selected Z.ai text model
produces an actionable error without switching models or billing routes.

Choose call, refinement-round and elapsed-time limits before starting. An
optional USD limit uses the existing spending tracker's effective-dated rates
and conservative input/output reservations. It is an estimate, not a billing
guarantee: tokenization, in-flight work and provider usage reporting can cause
an overrun. Unknown pricing and subscription usage are shown explicitly; use
call/time limits for these sessions. Planning, repair and review share the same
call gate and tracker attribution. Optional texture generation remains a
separate user action with the existing spending tracker.

**Stop** prevents further calls and captures. Local CLI subprocesses are killed
and reaped when cancellation or the deadline is observed. Blocking HTTP requests
may finish and incur usage; late responses cannot replace the source. Closing a
tab or Studio also cancels its session. Source/dependency checks prevent a result
from overwriting intervening edits. Every capture uses an immutable scene and a
fixed framing established at the start of the refinement comparison.

Select a uniquely named authored part, then **Focus edits** to preserve all
source text outside its subtree. Lock geometry, transforms, materials or a whole
subtree independently. Locks are checked against compiled geometry, world
transforms and resolved materials, so edits to parents, shared materials and
imported module definitions cannot bypass them by returning a full rewrite.
Dependencies are read-only during modeling sessions and must stay inside the
project directory. A source edit which needs to change a locked/shared part is
rejected with the existing source intact.

Candidates retain source, dependency bytes, reference/brief revision, model,
usage, views and findings. Review judgments are advisory. A worse or uncertain
later candidate does not automatically replace an earlier one. **Compare** shows
matching views and **Restore / Keep** integrates with undo/redo. If dependencies
have changed externally, **Restore as copy** writes a complete snapshot to a new
`.modeling-revisions` directory, preserving existing project assets; open the
reported source path to continue from it (`restored.mog`, or a numbered filename
when that name is already used by a dependency).

Saved files keep a versioned `<file>.mog.modeling.json` sidecar. Unsaved sessions
checkpoint under `~/.mogen/modeling-recovery/`; open the recovered `.mog` to
restore its sidecar. Embedded reference bytes survive removal of the originally
selected image. Missing dependency files and corrupt/unsupported sidecars are
reported; a corrupt sidecar is not silently overwritten. Scene Wizard object
generation writes the same context so opening a wizard object in Studio can
continue through the ordinary refinement workflow. Wizard review requires an
actual generated thumbnail and retains the original reference separately.

## Modeling operations

The application owns the iterative tool protocol in `mogen-llm::session`.
It supports inspecting named parts/hierarchy, world transforms, local bounds,
materials and diagnostics; retrieving task-relevant DSL documentation and
technique examples; atomic SEARCH/REPLACE or full-source edits; compilation;
and requested views, including named-part close-ups. Requests carry source/dependency revision hashes. Errors
return to the model for bounded recovery. The text JSON protocol is the
application-driven equivalent used across providers, including subscription
adapters; no shell access or provider-native tool capability is required.

The **experimental shape guidance** option separates hard language constraints
from modeling choices. It retains the validator-derived attribute allowlist and
coordinate conventions, and selects primitive assembly, authored placement,
modules, loft, sweep, lathe or organic geometry according to the target. Recipes
are retrieved by technique rather than injecting every example into every call.
This option is not a new default and has no claimed measured quality advantage.

## Evaluation and release comparison

Run the credential-free fixture evaluation from the repository root:

```sh
cargo run -p mogen --example quality_eval -- --out /tmp/mogen-quality
```

The six versioned tasks in `benches/quality/tasks.json` cover furniture,
upholstery, a hollow vessel, machinery, organic form and an assembled scene.
Original authored source and its derived reference renders use CC0-1.0.
The report records named-part/dimension/finite-geometry checks, export success,
matching-camera renders, prompts, settings, source, usage and termination. A
compiling model with missing required parts is an explicit negative control.
`--no-render` runs asset/export checks without a graphics context and clearly
omits the visual comparison. Rendering currently requires a working desktop GL
context (surfaceless Mesa/EGL works on Linux).

Live evaluations are opt-in and can consume API spend or subscription usage:

```sh
cargo run -p mogen --example quality_eval -- --live --variant baseline \
  --provider openai --model YOUR_MODEL --repeats 3 --calls 12 --seconds 600 \
  --out /tmp/mogen-baseline
cargo run -p mogen --example quality_eval -- --live --variant refined \
  --provider openai --model YOUR_MODEL --repeats 3 --calls 12 --seconds 600 \
  --out /tmp/mogen-refined
python3 scripts/compare-quality.py /tmp/mogen-baseline /tmp/mogen-refined \
  --out /tmp/mogen-blinded
```

Use `--variant guidance` as a separate comparison with the same model, task
manifest, seeds, temperature, thinking settings and budgets. Capture the baseline
before changing prompts. The prior prompt source and commit are preserved in
`benches/quality/baseline`; each live run also saves the exact assembled prompt.
Every run uses a new output directory so results cannot silently replace a prior
baseline. API credentials come from the provider's existing environment variable;
`--provider codex` uses its existing subscription route.

Open `review.html` in the comparison artifact. A/B assignments are deterministic
and hidden in `assignment-key.json`; keep that file and metadata away from
reviewers until they save judgments. The page downloads judgments with evaluator
identity, preference and free-text observations/uncertainty. Use multiple
reviewers, retain disagreements and record failures as well as improvements.
Report human preference, validity/repair rates, latency, recorded/unknown spend,
budgets and repeat counts. Do not substitute model self-scores or image similarity
for asset checks and human review. Attribute affected UV/water judgments to
renderer issues #105/#107 where appropriate, and turn genuine geometry gaps into
focused follow-up issues.

Release validation still requires live baseline/refinement/guidance runs and
independent human review. The included authored fixtures prove the evaluation
pipeline works; they do not establish AI modeling quality or a release threshold.
