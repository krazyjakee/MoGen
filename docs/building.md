# Building interior generation

This document is the long-lived plan for the `building` feature. The PR that
landed Tranche 1 covers a slice of the surface; the rest of this file maps the
full design so subsequent PRs can pick up without reconstructing the intent.

`building` is a top-level node kind that expands deterministically into a full
multi-room interior — walls, floor/ceiling slabs, openings, doors, windows,
skylights, stairs, elevators, and a roof — from a small set of high-level
attrs plus a seed. It is to architectural floorplates what `branch` is to
trees: one editable wrapper, with the entire subtree below it stamped
non-editable because the geometry is a pure function of the inputs.

The user-facing surface is **empty shells only**. Furnishing (beds, desks, …)
is not part of `building` — those compose via separate `use` calls inside the
rooms returned by the generator, exactly like `leaf` cards sit alongside
`branch`.

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
  room_type "corridor" (kind=public,  density=1, mat="plaster")

  adjacency "kitchen"  (adjacent_to=["living"], away_from=["bedroom"])
  adjacency "bathroom" (adjacent_to=["bedroom","corridor"])
}
```

### Top-level attributes

| attribute | default | unit / domain | effect |
|---|---|---|---|
| `seed` | `1` | `u32` | RNG seed; same seed = identical layout + geometry. |
| `style` | `"grid"` | enum | Layout algorithm. See [Styles](#styles). |
| `mat_style` | `""` | string | Free-text style hint forwarded to material/texture generation. Has no geometric effect. |
| `floor_area` | `120` | m², per floor | Target floorplate area; the layout solver picks an aspect ratio close to √2 unless `style` overrides. |
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
    │   │   └── connector "centre"    pos at room centroid
    │   └── …
    ├── openings (group)
    │   ├── door_<i> (group)          internal_door module instance
    │   ├── ext_door_<j> (group)      external_door module instance (ground floor)
    │   ├── window_<k> (group)        sized module instance
    │   └── skylight_<m> (group)      top-floor only
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
        2. sample_rooms_per_floor — distribute `rooms` across floors using
                                    floor_area weights and per-type density
        3. layout::solve         — for each floor, run style-specific
                                    subdivision, score adjacency, pick the
                                    best of N attempts
        4. emit::shell           — perimeter walls with hole specs for
                                    entrances + windows; slabs
        5. emit::rooms           — interior walls with door holes
        6. emit::openings        — instantiate door/window/skylight modules
                                    at hole centres
        7. emit::circulation     — staircase boxes (interior cutouts on
                                    each floor's slab), elevator shafts
        8. emit::roof            — style-driven roof geometry
        9. mark wrapper subtree non-editable
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

Tranches 2-4 implement these in priority order: **grid + apartment-block**
landed in Tranche 1; **hotel-corridor + office-core** landed in Tranche 3;
**radial, organic, maze** in Tranche 4 once the layout interface is proven.

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

## Roof (Tranche 4)

`roof="flat"`: top-storey ceiling slab is the roof; trivial.

For pitched/gabled/hipped/mansard/shed: the top slab is replaced by a roof
mesh generated via existing primitives (`prism`, `wedge`, `extrude`) and
parametrised by an internal `pitch_degrees=30` default.

`roof="gabled"`: two `wedge` meshes mirrored across the long axis, with two
triangular end walls extruded from the floorplate.

`roof="hipped"`: four `wedge` slopes meeting at a ridge or apex.

`roof="mansard"`: lower steep slope (60°) + upper shallow slope (15°), each
built from `frustum`/`wedge`.

`roof="shed"`: single `wedge` over the whole footprint sloping toward one
long side.

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
- `examples/apartment.mog`, `examples/grid_office.mog`.
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
- `examples/three_storey_house.mog` exercising every T2 feature
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
- Examples: `examples/small_hotel.mog`, `examples/office_core.mog`.
- Tests: hotel/office corridor-cell smoke, hotel corridor centred on
  long axis, `min_area` honoured, entrance-distance prior doesn't
  regress prior layouts.

### Tranche 4 — roof shapes + organics

- `pitched`, `gabled`, `hipped`, `mansard`, `shed` roof emitters.
- `radial`, `organic`, `maze` layout algorithms.
- Basement-specific layout (`floors_below > 0` may reuse the ground floor's
  footprint or a smaller cellar footprint via a `cellar_area=` attr added
  here).

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
  layout/corridor.rs     apartment-block with explicit central corridor (T2)
  layout/hotel.rs        hotel-corridor style (T3)
  layout/office.rs       office-core style (T3, thin wrapper over hotel core)
  emit/circulation.rs    stairs + elevators (T2)
  emit/skylight.rs       top-floor skylight cutouts + module stamps (T2)
  emit/wall_build.rs     wall-with-holes mesh assembly (T2 refactor)
```

Tranche 4 will add:

```
  layout/radial.rs
  layout/organic.rs
  layout/maze.rs
  emit/roof.rs          non-flat roof shapes
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
| Non-flat `roof=` | error `E1111` (pending tranche) | T1 |
| `style` outside `grid`/`apartment-block`/`hotel-corridor`/`office-core` | error `E1110` (pending tranche) | T1 (set widened in T3) |
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

1. `building_grid_smoke` — parse + lower `examples/grid_office.mog`, assert
   ≥ 4 wall nodes + 1 floor + 1 ceiling + ≥ 1 door instance.
2. `building_apartment_smoke` — same for `examples/apartment.mog`.
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

- **Furnishing**. `building` produces empty shells. Beds/desks/etc. compose
  via separate `use "bedroom_kit"` calls a user adds inside a downstream
  scene wrapper.
- **HVAC / plumbing / structural engineering**. Not represented in the scene
  graph at all.
- **Exterior cladding variations beyond `mat_style`**. Façade variety comes
  from the chosen materials, not from extra geometric layers.
- **Curved walls**. v1 is axis-aligned everywhere except in roof shapes.
- **Doors / windows with hardware**. The supplied module refs are
  responsible for their own swings, frames, handles, glass. v1 stdlib
  modules emit a flat panel + a frame box.
