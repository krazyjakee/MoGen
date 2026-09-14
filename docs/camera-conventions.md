# Asset front and matched comparison cameras

Camera convention **version 1** treats -Z as asset front, +Y as up. Front,
back and side remain cardinal views. `three_quarter` (also accepted as
`front_three_quarter`) and `presentation` look from the front-right quarter,
at yaw 3π/4 by default. `rear_three_quarter` explicitly selects the rear.
Yaw places the camera; it does not specify the look direction.

Use `meta(front="+z")`, `front="-x"`, or `front="+x"` for another world-space
front. With `meta(front="-z",front_node="assembly")`, front is the named
node's local axis transformed by its full parent chain, then projected onto
the horizontal plane. Missing/ambiguous targets and vertical/collapsed fronts
report E0140. Imported metadata does not override the root asset's metadata.

CLI thumbnail/refinement/publish, Studio presentation thumbnails/Frame,
modeling-session captures and the evaluator use this convention. Explicit
CLI yaw/pitch and manually saved viewport cameras remain explicit camera
choices. CLI thumbnails include a `.camera.json` sidecar with the exact
source/dependency revision and camera parameters.

## Comparison integrity

The first whole-asset capture fixes center, distance and asset-front yaw for
subsequent candidate comparisons. Each named view changes only its specified
angle. Changing asset dimensions or metadata in a later candidate does not
silently reframe the comparison. Actual indexed vertices are tested against
the capture camera's clip volume; crop counts and affected parts are saved.

A cropped comparison retains its original image and produces a separately
labeled `diagnostic_fit` image from the same immutable scene. The additional
image does not replace the matched comparison or change its stored camera.
Session review receives those images with explicit roles; Studio comparison
shows crop counts and an expandable diagnostic fit. Evaluation writes camera
JSON and diagnostic-fit PNGs beside candidate renders. Close-ups save/restore
whole-asset framing and orientation even when rendering returns an error.

Saved session views with no camera metadata remain identified as **legacy,
unversioned**. Their labels/pixels are preserved; they are never reclassified
as version 1. New artifacts store convention version, actual camera and exact
revision. Do not compare old and new presentation directions as matched views.

## Review fixtures

Render `examples/features/orientation_marker.mog` and the asymmetric
`examples/furniture/chair.mog` from front/back/side/front-three-quarter. The
red nose of the marker must face front and presentation; the blue fin marks
its rear. Repeat with front metadata and a rotated parent selected through
`front_node`.

Review targets (execution and rendered artifacts deferred):

```sh
cargo test -p mogen-core --test views
cargo test -p mogen-render --test capture_info
cargo run -p mogen --example quality_eval -- --out /tmp/camera-review
```

Keep baseline/candidate camera JSON together with renders when reviewing a
candidate that extends outside the fixed frame. Diagnostic-fit images are
for finding geometry outside that frame, not for judging relative scale.
