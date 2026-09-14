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

## Validation performed

The implementation intentionally minimized builds and tests at the user's request.
Nine focused core mesh-contract tests passed when that contract was introduced.
Scoped compile checks passed for DSL/validation, rendering/modeling libraries,
and finally the CLI plus quality evaluator. Diff whitespace checks passed.
The final CLI/example check emitted existing unused-mut exporter warnings.

Regression tests were added alongside the changes but their broader execution,
Studio builds, exported-artifact inspection, matched-camera renders, and aesthetic
acceptance remain for review. Compilation alone does not establish those outcomes.

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
