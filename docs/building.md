# Building interior generation

This document is the long-lived plan for the `building` feature. Tranches 1–4
have all landed; every layout style and roof shape originally specified is
implemented. The tranche schedule below stays in the doc as the change log;
the surface above the schedule describes the current behaviour.

`building` is a top-level node kind that expands deterministically into a full
multi-room interior — walls, floor/ceiling slabs, openings, doors, windows,
skylights, stairs, elevators, and a roof — from a small set of high-level
attrs plus a seed. It is to architectural floorplates what `branch` is to
trees: one editable wrapper, with the entire subtree below it stamped
non-editable because the geometry is a pure function of the inputs.

The user-facing surface is **empty shells** plus **furnishing markers**.
`building` never emits furniture *geometry* — but, like `cave`, it drops
geometry-free POI markers naming the props each room should hold (a `bed` in a
bedroom, a `stove` in a kitchen). A game-engine importer reads the marker
`role`/`tags` and swaps in its own prefab; the actual meshes still compose via
separate `use` calls or an engine-side furnishing pass, exactly like `leaf`
cards sit alongside `branch`. Set `furnish=0` to suppress the markers entirely.
See [Furnishing markers](#furnishing-markers).

---

## DSL surface

A complete `building` invocation:

```mog
building "house" (
  seed=1, style="apartment-block", mat_style="warm modern",
  floor_area=200,                 // m² target across all floors
  rooms=8,                        // total rooms to sample across floors
  floors_above=2, floors_below=0,
  windows=12, skylights=2,
  roof="gabled",
  ceiling_height=2.6,
  door_w=0.9, door_h=2.1,
  window_w=1.2, window_h=1.4,
  wall_thickness=0.12,
  ceiling_thickness=0.2,
  entrances=1,
  external_door="oak_door",       // module ref
  internal_door="plain_door",     // module ref
  window_small="win_s",           // module ref
  window_medium="win_m",
  window_large="win_l",
  skylight="sky_panel",
  elevators=0, staircases=1,
) {
  room_type "bedroom"  (kind=private, density=4, mat="warm wood")
  room_type "kitchen"  (kind=service, density=2, mat="tile")
  room_type "living"   (kind=public,  density=3, mat="oak")
  room_type "bathroom" (kind=service, density=2, mat="white tile")

  adjacency "kitchen"  (adjacent_to=["living"], away_from=["bedroom"])
  adjacency "bathroom" (adjacent_to=["bedroom"])
}
```

### Top-level attributes

| attribute | default | unit / domain | effect |
|---|---|---|---|
| `seed` | `1` | `u32` | RNG seed; same seed = identical layout + geometry. |
| `style` | `"grid"` | enum | Layout algorithm. See [Styles](#styles). |
| `mat_style` | `""` | string | Free-text style hint forwarded to material/texture generation. Has no geometric effect. |
| `floor_area` | `120` | m², per floor | Target floorplate area; the layout solver picks an aspect ratio close to √2 unless `style` overrides. |
| `cellar_area` | _unset_ | m² | Optional smaller footprint for basement storeys. When set, every storey with `floors_below` ≥ 1 uses this area instead of `floor_area`, east-aligned with the above-ground plate so the vertical-circulation column stays shared. Clamped to ≤ `floor_area`. |
| `rooms` | `4` | int ≥ 1 | Total rooms across **all** floors. Distributed proportionally to floor area. |
| `floors_above` | `1` | int ≥ 1 | Storeys above ground (incl. ground floor). |
| `floors_below` | `0` | int ≥ 0 | Basement storeys. |
| `windows` | `0` | int ≥ 0 | Total above-ground window count; the layout picks exterior wall edges and assigns a size class per window. |
| `skylights` | `0` | int ≥ 0 | Top-floor ceiling holes. |
| `roof` | `"flat"` | enum | `flat`, `pitched`, `gabled`, `hipped`, `mansard`, `shed`. |
| `ceiling_height` | `2.6` | m | Per-storey clear height. |
| `door_w`, `door_h` | `0.9, 2.1` | m | Internal door opening size. External doors reuse the same dimensions in v1. |
| `window_w`, `window_h` | `1.2, 1.4` | m | Window opening size (medium class). Small = 0.6×size, large = 1.4×size. |
| `wall_thickness` | `0.12` | m | Used for both exterior and interior walls. |
| `ceiling_thickness` | `0.2` | m | Slab thickness; the floor of storey N is also the ceiling of storey N-1. |
| `entrances` | `1` | int ≥ 1 | External door openings on the ground floor. |
| `external_door` | `"door_simple"` | module ref | Stamped at each entrance opening. |
| `internal_door` | `"door_simple"` | module ref | Stamped at each interior opening. |
| `window_small`, `window_medium`, `window_large` | `"window_simple"` | module ref | Stamped at window openings; the generator picks the size class. |
| `skylight` | `"skylight_simple"` | module ref | Stamped at each skylight opening. |
| `elevators` | `0` | int ≥ 0 | Vertical shafts spanning every storey. |
| `staircases` | `0` | int ≥ 0 | Stairwells spanning adjacent storeys (≥1 if `floors_above + floors_below > 1`). |
| `furnish` | `1` | bool (0/1) | Emit furnishing POI markers in each room (see [Furnishing markers](#furnishing-markers)). `0` leaves rooms as bare shells. Markers carry no geometry either way. |
| `debug_show_poi` | `0` | bool (0/1) | Debug: give every furnishing and door/window marker a small emissive sphere so the geometry-free POIs are visible in a glTF viewer, colour-coded by category/role. |
| `debug_hide_roof` | `0` | bool (0/1) | Debug: drop the top-storey ceiling slab (and its skylights) so the interior can be seen from above. |
| `debug_render_floor` | _unset_ | signed int storey | Debug: render only the given storey index (`0` = ground, `1..` = upper, `-1..` = basement). The rendered floor gets no ceiling and vertical circulation is skipped. |

### Child node kinds

`building` accepts only `room_type` and `adjacency` children. Everything else is
a validation error (caught by `validate_ast`).

#### `room_type "name" (kind=…, density=…, mat=…)`

| attribute | default | domain |
|---|---|---|
| `kind` | required | `public`, `private`, `service`, `utility`, `secure`, `staff_only` |
| `density` | `1` | int 0–10. Relative sampling weight when picking `rooms` instances from the declared types. |
| `mat` | inherits | string — material name. Used for the floor and walls of that room. |
| `min_area` | none | m². Optional lower bound the layout solver tries to honour. |
| `max_area` | none | m². Optional upper bound. |

#### `adjacency "name" (adjacent_to=[…], away_from=[…])`

Soft scoring rules. Each rule contributes to the layout-attempt score:

| attribute | default | effect |
|---|---|---|
| `adjacent_to` | `[]` | list of room-type names. +1 score per pair of rooms with these types sharing a wall. |
| `away_from` | `[]` | list of room-type names. −1 score per pair sharing a wall. |

The solver makes 10 layout attempts at different seeds derived from
`hash(seed, attempt_index)` and keeps the best-scoring one. Hard constraints
are deliberately rejected — LLM-authored rules over-constrain too easily.

---

## Output scene structure

Every `building "name"` lowers into a subtree like:

```
name (building)                       editable wrapper
└── floor_-1 (group)                  one per storey
    └── shell (group)
    │   ├── slab_floor (slab)
    │   ├── slab_ceiling (slab)
    │   ├── wall_N (wall)             perimeter walls with holes
    │   ├── wall_E (wall)
    │   ├── wall_S (wall)
    │   └── wall_W (wall)
    ├── rooms (group)
    │   ├── room_<n> (group)          one per room cell on this floor
    │   │   ├── interior_wall_<i> (wall)
    │   │   ├── connector "centre"    pos at room centroid
    │   │   └── furniture (group)     furnishing POI markers (if furnish=1)
    │   │       ├── bed_0 (poi)       role=bed, geometry-free
    │   │       └── …
    │   └── …
    ├── openings (group)
    │   ├── door_<i> (group)          internal_door module instance
    │   ├── ext_door_<j> (group)      external_door module instance (ground floor)
    │   ├── window_<k> (group)        sized module instance
    │   └── skylight_<m> (group)      top-floor only
    ├── opening_pois (group)          door/window POI markers (see below)
    │   ├── entrance_0 (poi)          role=entrance, geometry-free
    │   ├── door_0 (poi)              role=door, geometry-free
    │   └── window_0 (poi)            role=window, geometry-free
    └── circulation (group)
        ├── stair_<n> (group)
        └── elevator_<n> (group)
└── roof (group)                      style-driven; top-floor ceiling becomes roof
```

The wrapper carries the user's `pos=`/`rot=`/`scale=` like every other geometry
node. Everything inside is marked `editable=false` so the inspector won't let
users hand-edit cells that would be wiped on the next rebuild.

Every wall, slab, and module instance carries:

- `origin` inherited from the `building` node (so MoGen Studio's per-import
  sidebar groups them together);
- `tags` listing `building`, the storey index (`floor_0`), and a kind tag
  (`exterior_wall`, `interior_wall`, `entrance`, `window`, etc.) so downstream
  tooling (collider tags, lightmap groups, navmesh exclusion) can filter
  without re-parsing names;
- `role` set to the architectural role (`floor`, `ceiling`, `wall`, `door`,
  …) for the same reason.

---

## Furnishing markers

When `furnish=1` (the default), each real room (not circulation cells) gets a
`furniture` group of **points-of-interest**: transform-only nodes that name the
props a game engine should place there. This is the same contract `cave` uses
for mushroom spots and treasure rooms — the generator deliberately leaves the
gameplay content out and marks *where* it goes.

Each marker is:

- `kind = "poi"`, with **no mesh and no collider** (it adds nothing to the
  exported geometry or triangle budget);
- `role = "<prop>"` — `bed`, `stove`, `server_rack`, … — the name the importer
  keys a prefab off;
- `tags = ["building", "poi", "furniture", "<prop>"]`;
- a transform giving position **and yaw** in the room's local frame. Wall props
  face into the room, corner props face the centre, ceiling fixtures sit at
  `ceiling_height`.

The parent `furniture` group carries `cat=<category>` (e.g. `cat=kitchen`) so a
single tag lookup recovers what each room was furnished as.

### Categories

Which props a room gets is decided by **function**, not building type — the
author's free-text `room_type "name"` is keyword-matched onto a category, with
the declared `kind` as a fallback. The same tower can hold bedrooms, a server
room, and a lobby. Recognised categories (and a sample of their props):

| category | matches names containing… | sample props |
|---|---|---|
| bedroom | `bed`, `dorm`, `master`, `cabin`, `nursery` | bed, wardrobe, nightstand, dresser, desk |
| bathroom | `bath`, `toilet`, `shower`, `ensuite`, `wc` | toilet, sink, bathtub, shower, vanity |
| kitchen | `kitchen`, `galley`, `kitchenette` | counter, stove, oven, fridge, dishwasher, island |
| pantry | `pantry`, `larder` | shelving, dry-goods rack |
| dining | `dining`, `mess`, `canteen` | dining table, chairs, sideboard, chandelier |
| living | `living`, `lounge`, `den`, `family room` | sofa, armchair, tv, coffee table, fireplace |
| office | `office`, `study`, `cubicle`, `workspace` | desk, office chair, filing cabinet, whiteboard |
| meeting | `meeting`, `conference`, `boardroom` | conference table, chairs, projector screen |
| reception | `reception`, `waiting`, `concierge` | reception desk, waiting sofa, magazine rack |
| lobby | `lobby`, `foyer`, `atrium`, `entrance` | bench, planters, info board, directory sign |
| corridor | `corridor`, `hallway`, `landing`, `stair` | bench, wall art, fire extinguisher |
| storage | `storage`, `store`, `stock`, `supply` | shelving, storage boxes, pallets |
| closet | `closet`, `cloak`, `cupboard` | clothes rail, shelves, shoe rack |
| garage | `garage`, `carport`, `parking` | car, workbench, tool cabinet, pegboard |
| workshop | `workshop`, `maker`, `machine shop` | workbench, machine tool, material rack |
| laundry | `laundry`, `washing` | washing machine, dryer, ironing board |
| utility | `mechanical`, `boiler`, `hvac`, `electrical` | boiler, electrical panel, hvac unit |
| server room | `server`, `data centre`, `comms`, `rack room` | server racks, network cabinet, ups, crac |
| retail | `retail`, `shop`, `showroom`, `sales floor` | display shelves, checkout, clothing racks |
| warehouse | `warehouse`, `depot`, `loading`, `freight` | pallet racks, forklift, packing station |
| classroom | `classroom`, `lecture`, `seminar` | student desks, teacher desk, whiteboard |
| library | `library`, `archive`, `reading` | bookshelves, study tables, librarian desk |
| lab | `lab`, `research`, `cleanroom` | lab benches, fume hood, biosafety cabinet |
| medical | `clinic`, `exam`, `surgery`, `dental` | exam table, supply cabinet, exam light |
| ward | `ward`, `patient`, `recovery`, `icu` | hospital beds, iv stands, vitals monitors |
| gym | `gym`, `fitness`, `workout` | treadmills, weight benches, racks, mirror wall |
| restaurant | `restaurant`, `diner`, `cafe`, `coffee` | tables, chairs, booths, host stand |
| bar | `bar`, `pub`, `tavern`, `saloon` | bar counter, stools, back-bar shelf, beer tap |
| cell | `cell`, `holding`, `prison` | bunk, toilet, sink, stool |
| generic | _anything else_ | table, chairs, shelving, plant |

Prop counts scale with room area (a big office gets more desks, capped) and
small rooms drop the lower-priority items first; a room below ~0.5 m² of usable
floor is left unfurnished. Placement is a pure function of `seed` + the room
rectangle, so a rebuild reproduces every marker exactly and `lod_scale`-style
quality knobs never move them.

`debug_show_poi=1` gives each marker a small emissive sphere coloured by
category (kitchens orange, bedrooms pink, server rooms teal, …) so you can see
the furnishing plan in any glTF viewer. The spheres are a viewing aid only —
they keep the markers' `role`/`tags`, never get a collider, and must be turned
off for a production bake to keep the POIs geometry-free.

## Door & window POIs

Alongside the door/window *geometry* in the `openings` group, every floor also
gets an `opening_pois` group of **points-of-interest** — the same transform-only
contract as furnishing markers, but for openings. They make it easy to drop in
your own custom door/window prefabs without parsing the generated panel meshes:
read the marker pose, instantiate your prefab there, and (optionally) delete or
hide MoGen's default panel.

Each marker is:

- `kind = "poi"`, with **no mesh and no collider** by default;
- `role` = `entrance` (exterior door), `door` (interior door), or `window`;
- `tags` = `["building", "poi", "window"]` for windows, `["building", "poi",
  "door"]` for interior doors, and `["building", "poi", "door", "entrance"]` for
  exterior doors — so a generic "door" importer catches both door kinds while an
  entrance-only importer can still single out the street doors;
- a transform whose **position** sits at the opening's threshold/sill and whose
  local **+Z** points along the wall's outward normal — identical to the pose of
  the default module instance, so a prefab authored facing +Z at its base lands
  flush in the hole.

Door/window POIs are always emitted (independent of `furnish`); they only gain a
debug sphere under `debug_show_poi=1`, colour-coded by role (entrances orange,
interior doors yellow, windows cyan). Skylights keep their existing
`extras.slot` wrapper and are not given a POI marker. The opening's
width/height still live on the module-instance group's `slot` block; the POI
carries pose + role only, matching the furnishing-marker contract.

---

## Pipeline

```
ast::Node{kind="building"}
   │
   ├─ validate_ast::building_rules
   │     unknown child kinds, missing/typo attrs, unknown
   │     room-type refs in adjacency, count bounds (rooms ≥ 1,
   │     floors_above ≥ 1, densities ∈ 0..=10), enum values
   │
   └─ lower::building::expand_building(node, parent, graph)
        1. read_cfg              — AST attrs → BuildingCfg
        2. layout::solve         — compute bounds_above (and a smaller
                                    east-aligned bounds_below when
                                    `cellar_area` is set), plan the
                                    shared circulation column against the
                                    smaller of the two, then for each
                                    storey run the style-specific
                                    subdivision (best of N attempts under
                                    score::score).
        3. emit::shell           — perimeter walls with hole specs for
                                    entrances + windows; slabs (top slab
                                    is suppressed when roof != Flat).
        4. emit::rooms           — interior walls with door holes.
        5. emit::openings        — instantiate door/window/skylight
                                    modules at hole centres.
        6. emit::circulation     — staircase flights and elevator shafts
                                    spanning every storey.
        7. emit::roof            — Flat: no-op (slab IS the roof).
                                    Non-flat: one or more roof meshes
                                    plus gable end-walls.
        8. mark wrapper subtree non-editable.
```

Module instantiation reuses the existing module-expansion machinery: the
generator synthesises `use "<door_module>"(width=$door_w, height=$door_h)`
nodes and feeds them through `expand_modules` *before* the wrapper is closed.
Because building runs during `lower_into`, the registry passed in via the
lowering context is already populated with stdlib + user + import modules.

---

## Styles

Each `style=` selects one layout algorithm. All produce a list of rectangular
room cells in floor-local coordinates.

| style | algorithm | sketch |
|---|---|---|
| `grid` | uniform grid subdivision sized to room target | regular boxes; office-cube reading |
| `apartment-block` | BSP subdivision favouring aspect ratios ≤ 2.5 | typical flat layout |
| `office-core` | central corridor spine + perpendicular offices on both sides | open-plan core |
| `hotel-corridor` | single central corridor with rooms only on both long sides | hotel/dorm |
| `radial` | wedges fanning out from a central node, optional ring corridor | rotunda |
| `organic` | Voronoi from seeded cell centres, axis-aligned snap of edges | clinic / lobby |
| `maze` | recursive backtracker on a grid, walls between cells removed by spanning tree | maze interiors |

All seven styles ship after Tranche 4: **grid + apartment-block** landed in
Tranche 1; **hotel-corridor + office-core** in Tranche 3; **radial, organic,
maze** in Tranche 4. All cells remain axis-aligned rectangles — the more
"organic" styles approximate their conceptual shape under that constraint
(`radial` produces concentric rectangular bands; `organic` jitters grid
lines deterministically; `maze` extracts one full-axis corridor from a
spanning tree and emits the remaining cells as small rooms).

Each algorithm returns the same data:

```rust
struct Floorplate {
    bounds: Aabb2,                    // outer rectangle in floor-local space
    rooms: Vec<RoomCell>,             // axis-aligned rectangles, no overlap
}

struct RoomCell {
    rect: Rect2,
    typ: RoomTypeId,                  // index into BuildingCfg.room_types
    neighbours: Vec<RoomCellId>,      // adjacency graph via shared edges
}
```

The downstream emit pass is style-agnostic: it operates on `Floorplate`s.

---

## Adjacency scoring

For each layout attempt:

```
score = Σ adjacent_to_satisfied  −  Σ away_from_violated
        + Σ kind-based prior     (e.g. service near service, public near entry)
```

Layout attempts use seeds `seed`, `seed^0x1`, …, `seed^0x9`. The
highest-scoring attempt wins; ties are broken by lowest attempt index for
determinism.

`adjacent_to` and `away_from` are pure adjacency rules — they don't impose
ordering, distance, or co-floor constraints. If finer control is needed,
later tranches can add `on_floor=…`, `near_entrance=…`, `min_distance=…`
without breaking the existing surface.

Since Tranche 3 the scorer also bakes in:

- **Kind-based priors** that nudge same-kind cells together (service↔service,
  private↔private) and apart from contrasting kinds (public↔private,
  public↔secure). These apply on top of the explicit `adjacency` rules.
- **Distance-from-entrance** terms that pull `public` cells toward the
  south entrance and push `private` / `secure` cells away from it. Service
  cells prefer the middle of the plate.
- **Area-band penalties** when a cell's area falls outside its `room_type`
  `min_area` / `max_area` bound. Soft penalty (~0.2 per m² shortfall) so
  the solver still picks a slightly-too-small room over a feasible-but-
  badly-adjacent one.

All four terms (declared rules, kind priors, entrance distance, area band)
sum into a single score; the highest-scoring of 10 attempts wins.

---

## Vertical circulation (Tranche 2)

`floors_above + floors_below > 1` requires at least one staircase. The
generator places staircases as 2×3 m boxes that punch through every storey at
the same XY position, with the upper slab cut accordingly and a stair mesh
(`box` "treads") emitted inside.

Elevators are 2×2 m boxes spanning the entire vertical extent (basement to top
floor) with no internal mesh — the cab geometry is left to a user-supplied
`elevator_cab` module ref (added in Tranche 2).

Both circulation kinds need consistent XY positions across all storeys. The
solver promotes them to fixed "exclusion zones" that every floor's layout
must accommodate. If a chosen XY doesn't fit on some floor, the solver
re-attempts that floor with a different layout seed.

---

## Roof

`roof="flat"`: top-storey ceiling slab is the roof; trivial.

For every other shape the top-storey ceiling slab is **suppressed** and a
roof mesh is emitted in its place. Default pitch is 30° (`roof_h = 0.5 *
min(width, depth) * tan(30°)`); there is no per-roof attribute for pitch
in v1.

| `roof=` | construction |
|---|---|
| `shed` | one `wedge_mesh` spanning the whole footprint, sloping south→north |
| `pitched` / `gabled` | two `wedge_mesh` halves meeting at a ridge along the longer axis, plus two triangular end-walls extruded from `extrude_mesh` flush with the perimeter walls. `pitched` is a synonym of `gabled` in v1 — sloped end-faces would require non-axis-aligned vertices we have no representation for |
| `hipped` | one `frustum_mesh` whose top edge collapses to an apex (square footprint) or a ridge along the longer axis (rectangular footprint) — four slopes from a single watertight mesh |
| `mansard` | two stacked frustums: a steep ~60° lower tier and a shallow upper tier tapering to a short ridge (Second Empire profile) |

Every roof child carries `role="roof"`; gable end-walls carry
`role="gable_wall"`. The roof's base Y is `ceiling_height` (the top of the
perimeter walls), so the roof sits flush against the walls without a gap.

**Skylight × non-flat roof:** `skylights > 0` only works with `roof="flat"`
in T4 — the skylight planner short-circuits when the roof isn't flat and
the validator emits warning `W1114` so the author notices the silent drop.
Cutting holes through a sloped wedge would need CSG against the roof
mesh; that's saved for a future tranche.

---

## Tranche schedule

### Tranche 1 — minimum viable building (landed)

Scope:

- Grammar: no changes (uses existing `wall`, `slab`, list, list-of-string).
- AST: `building`, `room_type`, `adjacency` registered in validator schema +
  rules.
- `lower/building/` submodule with **single-floor only** layout for
  `style="grid"` and `style="apartment-block"`.
- Soft adjacency scoring across 10 attempts.
- Emit: floor + ceiling slabs, perimeter walls with entrance and window
  holes, interior walls with door holes, door / window module instances at
  openings.
- Roof: `flat` only.
- No elevators, no staircases (validation rejects nonzero counts in T1).
- Stdlib: `door_simple`, `window_simple`, `skylight_simple`.
- `examples/buildings/apartment.mog`, `examples/buildings/grid_office.mog`.
- `docs/dsl.md#building` section mirroring the Branch reference.
- Tests: golden-hash determinism, validation diagnostics, lowering smoke.

### Tranche 2 — vertical (landed)

Scope (delivered):

- Multi-storey iteration: `floors_above ≥ 1`, `floors_below ≥ 0`. Each
  storey gets its own `floor_<n>` group at Y = `n * (ceiling_height +
  ceiling_thickness)`. Basement storeys appear with a `b` prefix
  (`floor_b1`).
- Per-storey layout solve: rooms are sampled and distributed across
  storeys proportionally to storey count. Each storey runs its own
  scoring pass with a sub-seed derived from `seed + storey_mix`.
- Shared circulation: stairs and elevators are reserved in a column
  along the east edge of the floorplate before any storey's room
  layout runs, so they line up vertically across every storey. The
  room area passed to BSP/grid is `bounds` minus the circulation
  column.
- Slabs carved: a storey's floor slab gets CSG-subtracted by every
  circulation rect (except the bottommost storey, whose foundation
  stays intact). Top-storey ceiling slab is carved by skylight rects.
- Stair flights: one straight flight per storey transition (e.g. 3
  storeys → 2 flights). Stamped with the stdlib `stair_simple` module
  (or a synthetic step series if the module ref is missing).
- Elevator shafts: a single shaft spans the entire Y range with one
  `elevator_shaft_simple` module instance per building.
- Top-storey skylights: `cfg.skylights` rectangles distributed across
  room cells (no skylights over circulation cells). Same XY is carved
  through the roof slab and used to stamp the skylight module.
- Per-storey windows: `cfg.windows` distributed evenly across above-
  ground storeys with the remainder biased to lower floors. Basements
  receive no windows. Entrances stay on ground floor only.
- Validator: T1 gates relaxed; new `W1113` warning fires if
  `floors_above + floors_below > 1` and `staircases == 0` (the upper
  storeys would be visually disconnected).
- Stdlib: `stair_simple` (straight flight, tread-count derived from
  rise), `elevator_shaft_simple` (four-wall vertical column).
- `examples/buildings/three_storey_house.mog` exercising every T2 feature
  (basement + ground + 2 upper storeys, stairs, elevator, skylights).
- Tests: per-storey emission, Y stacking, stair count = N-1 per N
  storeys, elevator emits a single shaft, skylight only on top storey,
  upper floors have no entrance holes, stair XY consistent across
  storeys, deterministic mesh hash under same seed.

### Tranche 3 — remaining layout styles + better scoring (landed)

Scope (delivered):

- `style="hotel-corridor"` — single straight corridor along the longer
  axis with uniformly tiled side rooms (≥ 2.4 m run per room). Falls
  back to `grid` on floorplates too thin for a centre corridor + two
  room strips.
- `style="office-core"` — same machinery as hotel-corridor with a
  tighter target run (~3 m) so the same floorplate carries roughly
  twice as many cells per side. Reads as the rhythm of a typical
  office floor.
- Both styles synthesise a `corridor` room_type at `density=0` when
  the author doesn't declare one, so the corridor cell can be picked
  up by `pick_door_tree_root` and surfaced as `room_type=corridor`
  in tags.
- `min_area` / `max_area` honoured as a soft penalty (~0.2 per m²
  shortfall) in `score.rs::area_band_score`.
- Kind-based pairwise priors in `score.rs::kind_pair_prior` push
  service-service and private-private clusters together and
  public↔private / secure↔public pairs apart.
- Distance-from-entrance term in `score.rs::entrance_distance_score`
  pulls public cells toward the south entrance and pushes private /
  secure cells away from it. Service prefers the plate's middle.
- Validator: `BUILDING_STYLES_IMPLEMENTED` grew to include
  `hotel-corridor` and `office-core`; the "reserved for a future
  tranche" diagnostic now lists `radial`, `organic`, `maze` only.
- Examples: `examples/buildings/small_hotel.mog`, `examples/buildings/office_core.mog`.
- Tests: hotel/office corridor-cell smoke, hotel corridor centred on
  long axis, `min_area` honoured, entrance-distance prior doesn't
  regress prior layouts.

### Tranche 4 — roof shapes + organic styles + cellar (landed)

Scope (delivered):

- Five non-flat roof shapes (`pitched`, `gabled`, `hipped`, `mansard`,
  `shed`) implemented in `emit/roof.rs` from `wedge_mesh`,
  `frustum_mesh`, and `extrude_mesh`. The top-storey ceiling slab is
  suppressed for every non-flat roof so the roof mesh provides the
  upper closure of the volume.
- Three new layout styles (`radial`, `organic`, `maze`) in
  `layout/radial.rs`, `layout/organic.rs`, `layout/maze.rs`. All return
  axis-aligned `Vec<RoomCell>` and fall back to `grid::layout` on plates
  too small to honour their idiom.
- `cellar_area=` attribute: optional smaller footprint for basement
  storeys. East-aligned with the above-ground plate so the
  vertical-circulation column stays shared across every storey. Clamped
  to ≤ `floor_area` (a larger cellar can't exist beneath a smaller
  ground floor); the validator emits `W1116` if the author tries.
- New validator warnings: `W1114` (skylights are skipped under non-flat
  roofs), `W1115` (cellar too small for the circulation column),
  `W1116` (cellar larger than ground floor — silently clamped).
- `E1111` retired — every entry in `BUILDING_ROOFS` is now implemented.
- Examples: `examples/buildings/gabled_house.mog`, `examples/buildings/radial_lobby.mog`,
  `examples/buildings/mansard_brownstone.mog` (the brownstone exercises every T4
  axis in one file).
- Tests: per-roof smoke (gabled/hipped/mansard/shed), top-slab
  suppression, skylight-under-non-flat-roof warning, per-style layout
  smoke, basement-shrinks-only, basement-reuses-when-unset, and a
  cross-feature determinism check.

---

## File layout

All implementation lives under `crates/mogen-dsl/src/lower/building/`. The
800-line per-file ceiling is enforced for new code; each submodule has a
single responsibility:

```
crates/mogen-dsl/src/lower/building/
  mod.rs        ~200 LOC  public expand_building(); wrapper-node setup;
                          orchestrates config → layout → emit; non-editable
                          stamping.
  config.rs     ~300 LOC  AST → BuildingCfg + RoomType + Adjacency reading.
                          Hard-rejection of T1-out-of-scope attrs.
  rng.rs         ~60 LOC  Deterministic LCG identical to branch.rs's, with
                          hash(seed, attempt_index) helper.
  layout/
    mod.rs      ~150 LOC  Floorplate / RoomCell types + solve() dispatcher.
    grid.rs     ~200 LOC  Uniform grid subdivision.
    bsp.rs      ~350 LOC  Binary-space-partition for apartment-block.
    score.rs    ~150 LOC  Adjacency rule scoring on a Floorplate.
  emit/
    mod.rs      ~150 LOC  emit_floor(); orchestrator for the emit passes.
    shell.rs    ~300 LOC  Perimeter walls with hole carving.
    rooms.rs    ~250 LOC  Interior walls with door holes; per-room group
                          emission with materials.
    openings.rs ~200 LOC  Module instantiation for doors/windows/skylights.
    roof.rs     ~100 LOC  Tranche 1: flat only; stubs the other variants.
```

Tranches 2-3 added (each ≤ 800 lines):

```
  layout/hotel.rs        hotel-corridor style (T3)
  layout/office.rs       office-core style (T3, thin wrapper over hotel core)
  emit/circulation.rs    stairs + elevators (T2)
  emit/skylight.rs       top-floor skylight cutouts + module stamps (T2)
  emit/wall_build.rs     wall-with-holes mesh assembly (T2 refactor)
```

Tranche 4 added (all ≤ 800 lines):

```
  layout/radial.rs       concentric rectangular bands (T4)
  layout/organic.rs      jittered-grid Voronoi-ish layout (T4)
  layout/maze.rs         spanning-tree corridor + leaf-room cells (T4)
  emit/roof.rs           five non-flat roof shapes (T4 rewrite)
```

Each file ≤ 800 lines. If any approaches the cap, split by sub-concern
(e.g. `emit/shell.rs` could split into `shell_walls.rs` + `shell_slabs.rs`).

---

## Validation matrix

| condition | severity | tranche introduced |
|---|---|---|
| `building` has children other than `room_type`/`adjacency` | error | T1 |
| `room_type.kind` not in the allowed enum | error | T1 |
| `room_type.density` outside 0–10 | error | T1 |
| `adjacency` references an unknown room-type name | error | T1 |
| `floors_above < 1` | error | T1 |
| `floors_above ≥ 1`, `floors_below ≥ 0` (multi-storey) | valid since T2 | T2 |
| `floors_above + floors_below > 1` with `staircases == 0` | warning `W1113` (upper floors disconnected) | T2 |
| `staircases > 0` / `elevators > 0` / `skylights > 0` | valid since T2 | T2 |
| Non-flat `roof=` | ~~error `E1111`~~ retired in T4 — every shape is implemented | T1 (retired T4) |
| `style` outside the implemented set | error `E1110` (pending tranche) | T1 (set widened in T3 and T4) |
| `skylights > 0` with non-flat `roof` | warning `W1114` (skipped) | T4 |
| `cellar_area` too small for circulation column | warning `W1115` (lowering may bail) | T4 |
| `cellar_area > floor_area` | warning `W1116` (lowering clamps to `floor_area`) | T4 |
| `rooms < 1` | error | T1 |
| `floor_area` ≤ 0 | error | T1 |
| External/internal/window/skylight/stair/elevator module ref not found | error | T1 |
| Layout solver produced 0 rooms after all attempts | error | T1 |
| Adjacency rule mentions room-type whose count is 0 (after sampling) | warning | T3 |

All diagnostics carry the original AST span — building lowering is span-aware
through-and-through.

---

## Test plan

T1 must add:

1. `building_grid_smoke` — parse + lower `examples/buildings/grid_office.mog`, assert
   ≥ 4 wall nodes + 1 floor + 1 ceiling + ≥ 1 door instance.
2. `building_apartment_smoke` — same for `examples/buildings/apartment.mog`.
3. `building_deterministic` — same `seed=` produces identical mesh hashes
   across two `lower` calls.
4. `building_rejects_t2_features` — `floors_above=2` emits the expected
   "multi-floor support arrives in Tranche 2" diagnostic.
5. `building_unknown_module_ref` — `internal_door="not_a_module"` produces a
   span-tagged validator error.
6. `building_unknown_adjacency_target` — `adjacency` referencing an
   undeclared room type produces a span-tagged validator error.
7. `building_default_door_modules` — omitting all module refs falls back to
   the stdlib `door_simple` / `window_simple`.
8. `building_layout_scoring_prefers_satisfied_adjacency` — synthetic test:
   given two attempts where attempt A satisfies the rule and B violates it,
   the solver picks A.

Goldens: a per-building mesh-hash snapshot stored under `tests/goldens/` so
small algorithm tweaks surface in PR diff.

---

## Out of scope (forever, unless re-scoped)

- **Furnishing *geometry***. `building` produces empty shells. It marks where
  beds/desks/etc. belong (see [Furnishing markers](#furnishing-markers)), but
  the meshes themselves compose via separate `use "bedroom_kit"` calls or an
  engine-side pass that swaps each POI for a prefab.
- **HVAC / plumbing / structural engineering**. Not represented in the scene
  graph at all.
- **Exterior cladding variations beyond `mat_style`**. Façade variety comes
  from the chosen materials, not from extra geometric layers.
- **Curved walls**. v1 is axis-aligned everywhere except in roof shapes.
- **Doors / windows with hardware**. The supplied module refs are
  responsible for their own swings, frames, handles, glass. v1 stdlib
  modules emit a flat panel + a frame box.
