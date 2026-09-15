# Milestone 2 validation

Validation on Linux with Mesa software EGL, 2026-09-15. These artifacts separate
geometry and recovery correctness from model aesthetics.

## PR review fixes

Review found and fixed four session boundary problems:

- Missing or legacy execution state could make `--resume` start generation.
  Resume now requires saved execution settings; inspect requires a sidecar.
- Repeating an edit in a new refinement could select an older candidate and
  reuse its camera captures. Candidate reuse is now limited to the current run.
- A render-preflight failure before an input refinement started could lose the
  input on resume. The CLI now saves input source and request settings first,
  and clears inherited execution state when starting a new input refinement.
- Session reports/exports could overwrite input dependencies with matching
  names. Those collisions are rejected before copying dependencies.

The affected suites passed, including 353 LLM unit tests, 137 geometry unit
tests, 680 DSL unit tests, 107 validation unit tests, all 22 golden tests and
five CLI session tests. Additional integration tests passed; one pre-existing
CLI test remains ignored. `cargo check --locked -p mogen-studio` passed.

A final-binary Mesa smoke repeated the five-call generation/refinement run and
resumed it with an empty transcript, retaining exactly five charged calls and
the same selected revision. A separate input run deliberately failed render
preflight, then resumed with one scripted review and no generation call,
preserving the supplied source. These checks use no paid model calls.

## Deterministic checks

| Check | Result |
|---|---|
| `cargo test -p mogen-llm --lib` | 352 passed, including typed review normalization, format repair, raw DSL Apply, locks, cancellation, oversize retention and crash/resume |
| `cargo test -p mogen-geom -p mogen-dsl -p mogen-validate --lib` | 137 geometry, 680 DSL and 107 validation tests passed |
| `cargo test -p mogen --tests` | 34 passed; one existing ignored CLI test |
| `cargo check -p mogen-studio` | Passed; existing warnings remain |
| Existing `quality_eval`, fixture mode | Seven authored fixtures compiled/exported and rendered; negative mesh-contract controls detected |
| Scripted CLI, native renderer | Five calls: generate, findings-array review, raw DSL Apply, Finish, improved review; selected source and GLB exported |
| Scripted resume | Empty provider transcript suffices; five charged calls remain five; captures reused |
| MCP stdio smoke | `session` schema exposes documented parameters; inspect `structuredContent` equals the CLI report |

Crash injection exercises 35 checkpoint positions with the real atomic sidecar
save/load and fake provider/renderer. Completed response usage is not counted
twice. An interrupted admission reserves one uncertain call. A separate test
ensures a deadline during replay preserves the previously selected improvement.

Reproduce the scripted run with the JSON response array in
[`scripted-responses.json`](scripted-responses.json), then an empty array for
resume. Rendering needs the setup in the [CLI guide](../../../docs/modeling-session-cli.md).

```sh
mogen session --prompt 'a box' --script benches/quality/reliability/scripted-responses.json --out-dir output/scripted --glb
mogen session --resume --script benches/quality/reliability/empty-responses.json --out-dir output/scripted --glb
```

## Sports-body geometry

Open [matched before/after and subdivision views](compare.html), or compare
the PNGs directly. Before is base commit `6877349`; after uses this PR's final
geometry implementation. The same compact procedural source and all five
camera records are identical before/after. Neutral views expose the cut edges;
the presentation view retains glossy red paint. The severe triangular normal
artifacts are reduced while every triangle's positions, winding and UV corners
remain unchanged. Fine uneven edges remain visible; this is not a claim of
perfect surfaces or arbitrary production readiness.

The original body has 13,176 triangles, 28,358 render vertices and 6,578 exact
geometric positions. Its geometric topology is closed; render seams produce
boundary valences above two. The old boundary subdivision weights exceeded one,
causing spikes. The corrected CSG path checks geometric topology and uses convex
Loop masks, preserving separate UV interpolation.

| Subdivision, crease 40° | Render vertices | Triangles | Debug test time |
|---|---:|---:|---:|
| 0 | 28,358 | 13,176 | 1.35 s |
| 1 | 73,203 | 52,704 | 3.29 s |
| 2 | 190,573 | 210,816 | 10.58 s |

These timings include two compilations plus GLB export/readback and checks per
row on this machine; they are not isolated renderer or release benchmarks.
Vertex counts preserve existing UV/render splits rather than minimizing them.
The tests check finite unit normals, unchanged normal-only corner attributes,
triangle growth, bounded positions and the exported GLB. Subdivision retains
open cutouts but smooths/shrinks details. Regenerate levels by adding
`crease_angle=40, subdivide=1` or `subdivide=2` to `body_shell`; `quality_eval`'s
optional `candidate_source` compares each against the original source framing.
Reports are [fixture-comparison.json](fixture-comparison.json) and
[subdivision-report.json](subdivision-report.json). Their runtime paths describe
the temporary evaluator directories; the matched PNG/camera files are archived here.

Only `simple_house.glb` and `wall_door.glb` golden files change, for intentional
CSG final normals. All 22 golden tests pass. Unrelated W1208 warnings and UV
tiling remain outside this change.

## Opt-in live coupe retry

The [saved detailed brief](live-retry/brief.json) was run through ordinary
`mogen session`, Codex subscription / `gpt-6-astra`, with the original retry's
limits: **24 calls, 4 iterations, 2,400 seconds, 24,000 output tokens per call**,
no USD cap and default sampling. It stopped at 24 calls after **1,964 seconds**.
Reported usage: 2,737,585 input tokens, 52,227 output tokens, 25,344 cached tokens;
subscription cost is unknown, not $0. The selected GLB exported successfully.

The initial source failed on nested parameterized vectors. Automated tool
repair produced a valid candidate; another rejected Apply referenced a missing
connector. The typed visual review then reported angular bodywork, protruding
lights and unfinished detail integration. Three further edits passed Apply
validation. The call budget expired before a completed correction/review cycle,
so the earlier reviewed candidate remains selected and the latest edit is
explicitly unreviewed. Sources and response outcomes are retained in
[`live-retry/`](live-retry/); the run report preserves the original output layout
under `original-run/` rather than asserting those temporary files are portable.

There were **no manual source edits, response repairs or prompt interventions**
during this retry. Environment preparation supplied Mesa library/driver paths
and the existing Codex authentication. Afterward, the evaluator rendered the
selected and latest source with matched cameras for the
[comparison](live-retry/compare.html). Those captures use the final geometry
code; the model run itself used an earlier development build of this PR, before
the final roundoff seam, sampling persistence, stopped-response receipt and
replay-selection checks. The final code is covered by deterministic tests and
the scripted CLI smoke; this is not an exact-commit live certification.

Visual inspection still shows an angular, retro-style body and cabin, limited
light integration and uneven details. The latest edit has smoother shoulders,
but this one unblinded run does **not** establish an aesthetic improvement or
satisfaction of the original sports-coupe brief.
