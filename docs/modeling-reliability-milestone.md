# Predictable DSL Modeling & Geometry Reliability: review map

All eight issues in [milestone #1](https://github.com/krazyjakee/MoGen/milestone/1)
have implementations in draft PRs. Review, acceptance testing, and merge remain
pending. This document does not mark the milestone accepted or closed.

## Groups and review order

| Group | Issue | Draft PR | Change |
| --- | --- | --- | --- |
| 1: Geometry foundation | #120 | [#128](https://github.com/krazyjakee/MoGen/pull/128) | Retain computed sweep, loft and extrude normals |
| 1: Geometry foundation | #121 | [#129](https://github.com/krazyjakee/MoGen/pull/129) | Shared renderable mesh contract at consumer boundaries |
| 2: Predictable authoring | #122 | [#130](https://github.com/krazyjakee/MoGen/pull/130) | Consistent numeric arrays, including three values |
| 2: Predictable authoring | #123 | [#131](https://github.com/krazyjakee/MoGen/pull/131) | Explicit sweep frames and closed-path continuity |
| 2: Predictable authoring | #127 | [#132](https://github.com/krazyjakee/MoGen/pull/132) | Canonical asset front, capture metadata and crop diagnostics |
| 3: Relationships and fit | #124 | [#133](https://github.com/krazyjakee/MoGen/pull/133) | Deterministic endpoint, alignment and grounding relationships |
| 3: Relationships and fit | #125 | [#134](https://github.com/krazyjakee/MoGen/pull/134) | Reusable cushion and frame surface guides |
| 3: Relationships and fit | #126 | [#135](https://github.com/krazyjakee/MoGen/pull/135) | World measurements and selectable triangle-surface gaps |

#128 targets master. #129 depends on the existing Generate and Refine work in
[#119](https://github.com/krazyjakee/MoGen/pull/119), and includes #128's normal fix.
Review/merge #128 and #119 first. #130–#135 are stacked in the order above, each
against its predecessor. Retarget each next PR after its parent merges; use its
listed base branch while reviewing to avoid counting earlier changes again.

The final integrated branch is `feat/fit-measurements`. The implementation used
isolated worktrees; pre-existing changes in the original checkout were preserved.

## Review validation (2026-09-15)

Review covered #119, #128–#135 and the independent lowering PR #109.
Xiaomi support (#87) was excluded at the user's request. No PR was merged or
marked ready, and no live provider requests were made.

Blockers fixed and propagated through the stack:

- **#128:** updated the gabled-house golden's repaired NORMAL accessor bytes.
  All other bytes were retained, including positions, indices and UVs.
- **#129:** normalized surface-net field gradients before storing blob normals.
  The new contract previously rejected the ordinary organic quality target.
  Added coverage at three grid resolutions; connectivity tests now assert
  connectivity separately from advisory pole-degeneracy diagnostics.
- **#130–#131:** shortened redundant primitive examples to retain numeric-array
  and explicit-frame guidance within the existing 34,000-byte prompt budget.
- **#133:** replaced unsupported unary parameter negation in the chair fixture
  with supported subtraction expressions. Parameter-sweep and fit tests pass.
- **#134:** measured cushion guide error against the nearest point on each
  chord, rather than equal parameter fractions, and evaluated cardinal angles
  accurately. Fractional-power curves no longer exhaust the sample limit due
  to nonuniform speed. A dense 4,096-point analytic check verifies tolerance.

`cargo test --workspace --no-fail-fast -- --test-threads=1` passed with
**1,943 tests and two existing ignores**, including Studio, session, geometry,
validation, export and all 22 goldens. The additional dense guide regression
then passed in the four-test `surface_guides` target. `git diff --check` passed
for every updated branch. Existing compiler warnings remain.

The credential-free quality evaluator passed compilation, dimensions, required
parts, mesh-contract and GLB export checks for all six authored quality targets,
and produced their five canonical views using software Mesa/EGL. Six further
fixtures (profile normals, explicit sweep frame, orientation marker, relational
chair, guided cushion and frame trim) also passed asset/export checks and rendered
five views each. The deliberately disconnected joint-measurement fixture is
covered by measurement tests; strict rendering correctly rejects its untagged
disconnected clusters. The compiling-but-incomplete negative control and
malformed mesh controls were detected.

#109's DSL and CLI suites passed after incorporating current master: 719 tests,
one existing ignore, including all 22 goldens. Earlier golden failures on its
outdated base no longer occur.

These are implementation and authored-fixture checks. Live-provider comparisons,
independent human judgments and milestone acceptance remain pending; the runs
do not establish generated-model quality.

## Review guides and fixtures

- [Profile normals](profile-normals-regression.md): constructor/export regressions
  and `examples/features/profile_normals.mog`.
- [Renderable mesh contract](renderable-mesh-contract.md): diagnostic policy and
  malformed-mesh controls across core, export, render and session boundaries.
- [Numeric arrays](dsl.md#scalar-arrays-and-coordinate-lists): three-value and malformed-array coverage.
- [Sweep frames](sweep-frames.md): explicit orientation and closed-loop fixtures.
- [Camera conventions](camera-conventions.md): canonical directions, fixed
  comparison framing, legacy captures and separate diagnostic-fit images.
- [Relationships](relational-modeling.md): the parameterized relational chair and
  dependency/lock regressions. Export merging preserves semantic owners.
- [Surface guides](surface-guides.md): parameterized cushion welt, frame trim,
  tessellation-independent references and supported-shape limits.
- [Fit measurements](fit-measurements.md): joint fixture, closest-point evidence,
  transformed constraints, intentional overlap policy and bounded search.

The quality manifest requests surface measurements for selected chair and
upholstery pairs. Geometry evidence and authored constraints are reported
separately from visual quality; no goldens were regenerated automatically.
