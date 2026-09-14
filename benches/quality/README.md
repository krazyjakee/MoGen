# Modeling quality tasks

See [Generate and Refine evaluation](../../docs/generate-and-refine.md#evaluation-and-release-comparison)
for the fixture command, opt-in live runs and blinded comparison procedure.

`targets/` contains original, deliberately compact diagnostic assets under
CC0-1.0. Their generated images inherit that dedication. These are reference
fixtures and technique examples, not model-generated baseline measurements.
`baseline/` preserves the prompt source and repository revision observed before
this implementation. No live provider results or human preferences are invented.

`reference-views/` contains the five fixed views for each authored target;
`fixture-report.json` records the executed asset/export checks and
`negative-control.json` records rejection of the incomplete but compiling model.
These artifacts were produced with software Mesa/EGL at 512 × 512 pixels.
