//! System-instruction assembly for Gemini.
//!
//! The system instruction is the static context we want cached: DSL grammar
//! rules, known node kinds/attributes, and a summary of shipped stdlib modules
//! (if any). Keeping this assembly pure lets callers hash it and pin a
//! `cachedContents` resource on the Gemini side.

use mogen_dsl::ModuleRegistry;
use mogen_validate::{attrs_for_kind, KNOWN_KINDS};

/// A light summary of modules discovered in stdlib / user module paths.
/// Used purely to populate the system instruction — the real registry is
/// resolved at lowering time.
#[derive(Debug, Default, Clone)]
pub struct StdlibIndex {
    pub modules: Vec<ModuleSummary>,
}

#[derive(Debug, Clone)]
pub struct ModuleSummary {
    pub name: String,
    pub params: Vec<(String, Option<String>)>,
    /// Optional one-line description (e.g. from a leading `//` comment in the
    /// module's source file).
    pub doc: Option<String>,
}

impl StdlibIndex {
    pub fn from_registry(reg: &ModuleRegistry) -> Self {
        let mut modules: Vec<ModuleSummary> = reg
            .names()
            .map(|name| {
                let def = reg.get(name).expect("registry name disappeared");
                ModuleSummary {
                    name: def.name.clone(),
                    params: def
                        .params
                        .iter()
                        .map(|p| {
                            let default = p.default.as_ref().and_then(|e| e.eval_const())
                                .map(|n| format_number(n));
                            (p.name.clone(), default)
                        })
                        .collect(),
                    doc: def.doc.clone(),
                }
            })
            .collect();
        modules.sort_by(|a, b| a.name.cmp(&b.name));
        Self { modules }
    }

    pub fn is_empty(&self) -> bool {
        self.modules.is_empty()
    }
}

/// Render the validator's `attrs_for_kind` table into the prompt as a closed
/// allowlist. Sorted alphabetically for deterministic cache-key output.
fn render_allowlist(s: &mut String) {
    let mut kinds: Vec<&str> = KNOWN_KINDS.iter().copied().collect();
    kinds.sort_unstable();
    for kind in kinds {
        let attrs = attrs_for_kind(kind);
        s.push_str(&format!("- `{kind}`: "));
        if attrs.is_empty() {
            s.push_str("(no kind-specific attrs)");
        } else {
            let joined = attrs
                .iter()
                .map(|a| format!("`{a}`"))
                .collect::<Vec<_>>()
                .join(", ");
            s.push_str(&joined);
        }
        s.push('\n');
    }
}

fn format_number(n: f32) -> String {
    if n == n.trunc() && n.abs() < 1e6 {
        format!("{}", n as i64)
    } else {
        format!("{n}")
    }
}

/// Build the system instruction. Stable byte-for-byte given the same index —
/// this matters for cache keying.
pub fn system_instruction(index: &StdlibIndex) -> String {
    let mut s = String::with_capacity(8192);
    s.push_str(PREAMBLE);
    s.push_str("\n\n## DSL grammar (pest-derived, informal)\n\n");
    s.push_str(GRAMMAR_REFERENCE);
    s.push_str("\n\n## Conventions: units and orientation\n\n");
    s.push_str(CONVENTIONS);
    s.push_str("\n\n## Known node kinds\n\n");
    s.push_str(KINDS_REFERENCE);
    s.push_str("\n\n## Attribute allowlist (authoritative)\n\n");
    s.push_str(ALLOWLIST_INTRO);
    render_allowlist(&mut s);
    s.push_str("\n\n## Prompt → DSL demonstrations\n\n");
    s.push_str(FEWSHOT);
    s.push_str("\n\n## Stdlib modules\n\n");
    if index.is_empty() {
        s.push_str("_No stdlib modules are currently registered. Define any you need inline with `module \"name\" (...) { ... }` and then instantiate with `use \"name\" (...)`._\n");
    } else {
        s.push_str("When one of these modules fits a part, prefer `use \"name\" (...)` over rebuilding it from primitives — they're pre-validated.\n\n");
        for m in &index.modules {
            s.push_str(&format!("- `{}(", m.name));
            let parts: Vec<String> = m
                .params
                .iter()
                .map(|(n, d)| match d {
                    Some(v) => format!("{n}={v}"),
                    None => n.clone(),
                })
                .collect();
            s.push_str(&parts.join(", "));
            s.push_str(")`");
            if let Some(doc) = &m.doc {
                s.push_str(" — ");
                s.push_str(doc);
            }
            s.push('\n');
        }
    }
    s.push_str(OUTPUT_CONTRACT);
    s
}

const ALLOWLIST_INTRO: &str = "\
This is the closed list of kind-specific attributes. Do NOT invent others.

**Common attributes** (any geometry/group — `scene`, `group`, `solid`, \
`stack`, `grid`, `mirror`, `array`, `union`/`difference`/`intersect`, \
`module`/`use`, and every primitive): `pos`, `rot`, `scale`, `mat`, \
`role`, `tags`, `skin`, plus the placement shortcuts (`x`/`y`/`z`, \
`rx`/`ry`/`rz`, `w`/`h`/`d`, `anchor`, `above`/`below`/`left_of`/\
`right_of`/`in_front_of`/`behind`, `gap`, `from`/`to` corner shortcuts).

Common attributes do **NOT** apply to `material`, `connector`, `attach`, \
`joint`, `clip`, `track`, `skeleton`, `bone`, or animation templates \
(`spin`, `open_close`, `wave`, `flap`, `idle`). Those kinds accept ONLY \
what's listed below — in particular, animation templates take no \
`from=`, `to=`, `pivot=`, or `offset=`. To rotate something around a \
non-centre point (hinge, wrist, shoulder), wrap the target in a `group` \
whose origin sits at the pivot and target the `joint`, not the mesh.

";

const PREAMBLE: &str = "\
You are an expert 3D technical artist that converts short natural-language \
prompts into `mogen` DSL files that compile to engine-ready glTF assets. You \
reason about anatomy, proportion, and how parts join before you place them.

Follow these rules in order — they are what separate a first-try compile \
from a validator failure.

1. Root the scene in a single `scene { ... }`. Inside, declare each logical \
   part (\"body\", \"leg\", \"wheel\", \"rotor\") as a named primitive built \
   at the origin in canonical orientation (see Conventions), sized by its \
   own `size` / `radius` / `height`.
2. Join parts with `attach`, never with hand-computed `pos=` arithmetic. \
   Hand-computed positions are the #1 source of floating-head and \
   disconnected-limb bugs; `attach` exists to avoid them.
3. Reserve `pos=` / `rot=` for deliberate spacing of parts that are NOT \
   joined: a rotor hub offset from its tower, a planet's orbit radius, a \
   floating UI element. **Concentric parts — a window pane inside its \
   frame, liquid inside a glass, a core inside a shell, an inner ring \
   inside an outer — share the same default origin and need NO `attach`. \
   `attach` pins surface-to-surface; it cannot concentrate two shapes, \
   and applying it (e.g. `socket=\"top\", plug=\"top\"`) stacks the pane \
   above the frame instead of seating it inside.**
4. A geometric connectivity validator runs after lowering — every \
   primitive's world-space bounding box must touch another (within 2 mm) or \
   the scene fails with diagnostic E1101. If the gap is intentional (drone \
   rotor, chandelier, orbiting body), add `tags=\"floating\"` on the \
   primitive or an ancestor group to exempt the subtree.
5. Declare a `material` for every surface colour you reference with `mat=`. \
   Every `mat=\"name\"` must match a `material \"name\" (...)` declared in \
   the same file. When you assign image textures, set `uv_mode` explicitly: \
   `uv_mode=\"tile\"` (default — repeating surfaces like stone, wood, \
   fabric, ground, shingles; texel density stays constant across \
   primitives because UVs are world-space) or `uv_mode=\"fit\"` (decals, \
   signs, paintings, stained-glass images, anything where the texture *is* \
   the picture and must land once on each face). Tune density with \
   `uv_scale=N` (tiles per unit) when bricks/planks should be larger or \
   smaller; default `1.0`.
6. For unspecified \"animate it\" prompts, prefer a procedural template \
   (`spin`, `open_close`, `wave`, `flap`, `idle`) over hand-authored \
   `joint` + `clip` + `track`. Reach for the full joint/clip machinery only \
   when the prompt demands specific keyframed motion.
7. Organic subjects that bend — humanoids, creatures, arms, tails — need a \
   `skeleton { bone ... }` rig with mesh parts bound via `skin=\"<rig>\"`. \
   Drive multi-limb cycles (walk, wave, idle flex) with a **single** `clip` \
   containing one `keys=[[t, deg], ...]` `track` per bone so limbs stay in \
   phase. Mechanical/architectural subjects (chairs, cars, buildings) \
   stay as rigid `attach`-joined primitives with per-part procedural \
   animation — do not rig them.
8. **Organic shapes** (people, animals, plants): prefer `use \"humanoid_*\"` \
   / `use \"quadruped_*\"` from the stdlib over rebuilding limbs from raw \
   primitives — the stdlib parts already smin-blend their internal joints \
   (no shoulder or knee crease). For **whole trees / large bushes** reach \
   for the recursive `branch (...)` node — one declaration emits a tapered \
   trunk + recursive forks + alpha-cutout `leaf_card` foliage at every \
   tip; pair it with `material (alpha_mode=\"mask\", double_sided=1)` for \
   the leaf material. Use `use \"leaf\"` only when you need a single \
   curved-plane leaf cluster on something that *isn't* a procedural tree \
   (potted plant, single hanging vine). When two parts of one body must visually merge into one \
   surface — neck-into-torso, hip-cap-into-leg, jaw-into-skull — wrap them \
   in `union \"joint\" (smooth=K) { ... }` with `K ≈ 0.04–0.10` for \
   human-scale parts (`K` is a fillet radius in metres; too large and the \
   parts melt together, too small and the seam stays visible). Allow \
   ±3 % asymmetry on paired parts (one ear/leg slightly different) — \
   biology is never perfectly mirrored. Material naming: \
   `<creature>_<region>_<surface>` (e.g. `tiger_back_fur`, `oak_bark`, \
   `koi_belly_scales`) so the texture pipeline picks anatomical priors \
   when generating the albedo.";

const GRAMMAR_REFERENCE: &str = "\
A `.mog` file is a sequence of nodes. Each node is:

    kind [\"optional name\"] [(attr=value, ...)] [{ child_nodes... }]

- Values: numbers, vec3 `[x,y,z]`, strings, idents, simple arithmetic with \
  `$param` references inside `module` bodies. Comments start with `//`.
- `pos`, `rot` (Euler XYZ degrees), `scale`, `mat`, `role`, `tags` are \
  accepted on any geometry/group node.
- Placement shortcuts — use these instead of hand-computing `pos`: \
  `x=/y=/z=` and `rx=/ry=/rz=` set one component of `pos`/`rot` (other \
  axes fall back to the vec3 or `0`); `w=/h=/d=` override one component \
  of `size`; scalar `size=N` is a uniform cube; `from=[x,y,z], \
  to=[x,y,z]` sets both `size` and `pos` from corner points. \
  `anchor=bottom|top|left|right|front|back` (or underscored combos like \
  `bottom_left_front`) controls where `pos` lands on the primitive — \
  default `center`. `anchor=bottom` is the \"sits on the ground\" \
  shorthand. Default connectors shift with the anchor so attach math \
  stays correct.
- Relative placement: `above=\"sib\"`, `below=\"sib\"`, \
  `left_of=\"sib\"`, `right_of=\"sib\"`, `in_front_of=\"sib\"`, \
  `behind=\"sib\"` (+ optional `gap=N`) flush the node against a prior \
  sibling's opposite face — the sibling's whole subtree is included in \
  the AABB. Use for hat-on-head, shelf-above-shelf, tier-on-cake, \
  door-in-wall; one per node, sibling-scoped. Cheaper and safer than \
  reaching for `attach` when two parts just need to sit next to each \
  other.
- `connector \"name\" (at=[...], dir=[0,1,0], tag=anchor, radius=0.05)` \
  declares a named attach point. Every primitive already exposes canonical \
  connectors (see Known node kinds) — add a `connector` yourself only when \
  you need an off-default anchor (e.g. a wheel mount on the side of a car \
  body).
- `attach (parent=\"p\", child=\"c\", socket=\"top\", plug=\"bottom\", offset=0, twist=0)` \
  snaps `child`'s plug to `parent`'s socket (plug pointing anti-parallel \
  into the socket) and reparents `child` under `parent`. Default \
  socket/plug are `top`/`bottom`. `offset` slides along the socket \
  direction (negative embeds the child); `twist` rotates around it.
- `mirror axis=x|y|z { ... }` reflects its children; \
  `array count=N around=y|x|z { ... }` repeats around an axis. \
  `stack (axis=y, gap=0, align=center, pack=start) { ... }` lays children \
  out along `axis` by their AABB slots (no accumulated `pos` math — \
  reach for this before writing `pos=y` arithmetic). `grid (count=[x,y,z], \
  step=[x,y,z], center=0|1) { ... }` replicates across a lattice; a \
  scalar `count`/`step` applies to X, a 2-element list to X/Z.
- `solid { ... }` behaves like `group` in the scene tree, but its \
  same-material, non-skinned **direct leaf** children are CSG-unioned \
  into one watertight mesh at export (leaves nested inside another \
  `group`/`stack`/`array` are NOT merged — keep the primitives as \
  direct children of `solid`). Use for multi-primitive shells of a \
  single material (stone hut walls, archway, tower, chimney) so \
  interior faces vanish and the whole shape reads as one surface. \
  `cleanup=\"coplanar\"` additionally drops opposite-facing coplanar \
  triangle pairs where boxes merely *touch* without overlapping \
  (perpendicular walls meeting at a corner).
- `module \"name\" (param=default, ...) { body }` + `use \"name\" (arg=value, ...)` \
  parameterises a sub-graph. `$param` inside the body substitutes the arg.
- CSG: `union { ... }`, `difference { base cutouts... }`, `intersect { ... }`. \
  The CSG node's own `mat=` wins; if absent it inherits the first operand's \
  material. Operand materials are otherwise discarded. Cut tools must **fully \
  pass through** the surface they cut (a subtractor whose flat endcap stops \
  inside the solid leaves a stray face) and must avoid **coplanar faces** with \
  the base (offset concentric cavities by ~1% of their size along the shared \
  plane — e.g. nudge an inner hemisphere used to hollow out a dome by \
  `pos=[0,-0.01,0]` so its base cap isn't coplanar with the outer one).
- Animation: `joint \"name\" (type=hinge|slider|ball|rotor, axis=[...], \
  pivot=\"node\", limits=[lo,hi])` + \
  `clip \"name\" (seconds=N) { track \"j\" (from=0, to=90) }`. \
  Procedural templates: `spin` (axis, rpm), `open_close` (axis, angle, \
  seconds), `wave` (axis, amplitude, hz), `flap` (axis, amplitude, hz), \
  `idle` (amplitude, hz — breathing scale, no axis/angle). Each takes \
  `target=\"node\"`. For a gentle sway (trees in wind, flags) use `wave` \
  with a small amplitude, not `idle`.
- `track` has two forms. `from=A, to=B` emits a 2-keyframe linear track \
  from 0s to the clip's `seconds=`. `keys=[[t, v], [t, v], ...]` emits \
  one keyframe per pair — `t` is absolute seconds, `v` is degrees (for \
  `prop=rotation`), meters (`prop=translation`), or a uniform scale \
  factor (`prop=scale`). Use `keys=` for cyclical motion and to \
  coordinate multiple bones in one clip — end the cycle on the same \
  value as the start (`[0, -25], [0.5, 25], [1.0, -25]`) so the loop \
  is seamless. `axis=[x,y,z]` picks the rotation axis for direct-node \
  tracks (hips usually rotate around `[1, 0, 0]`).
- Rigging: `skeleton \"rig\" { bone \"root\" (pos=[...], envelope=0.2) \
  { bone \"child\" (pos=[...]) } }`. Bones nest; each child bone's \
  `pos` is the offset **from its parent bone's joint**, not from the \
  skeleton root. Mesh primitives carry `skin=\"rig\"` to be bound by \
  nearest-bone + linear envelope falloff (max 4 influences per vertex); \
  envelope is the binding radius in meters — roughly the limb's width \
  (0.15–0.25 for a humanoid limb on a 1.7m figure). A skinned mesh's \
  own transform is baked into world space at bind time, so place the \
  mesh in its final world position and let the rig take over from there.";

const CONVENTIONS: &str = "\
**Frame.** +Y up, -Z forward (the direction something faces), +X right. So \
`top` faces +Y, `front` faces -Z, `right` faces +X.

**Canonical orientation for parts built at the origin:**

- `cylinder`, `cone`, `capsule`, `pyramid`, `tube`, `half_cylinder`: `height` \
  along Y. Default connectors `top` (+Y end) and `bottom` (-Y end) at \
  ±height/2.
- `box`, `rounded_box`, `prism`, `wedge`, `ellipsoid`: `size=[x, y, z]` is \
  `[width, height, depth]`.
- `frustum`: `bottom=[x,z]` / `top=[x,z]` are the two end rectangles; \
  `height` runs along Y. Either end may be larger.
- `wedge`: doorstop shape — tall wall at -Z, slopes down to ground edge at \
  +Z. Use for car hoods, ramps, windshields.
- `tube`: hollow cylinder along Y. `inner < outer`. Use for wheel rims, \
  rings, pipes, gun barrels.
- `hemisphere`: flat base at y=0 (origin = base centre), dome to y=+radius. \
  Use for headlight lenses, domes, nose cones. Stacks cleanly onto any \
  surface via `bottom`/`base`.
- `half_cylinder`: flat rectangular face on YZ plane at x=0, curve bulges +X. \
  Use for wheel arches, barrel vaults.
- `torus_arc`: partial torus sweeping `arc` degrees (default 90) around +Y \
  starting at +X. End caps close the tube. Use for bent pipes, wraparound \
  fenders.
- `ellipsoid`: prefer over non-uniformly-scaled `sphere` when children are \
  attached — scaling a parent propagates into the attach joint and warps \
  descendants.
- `superellipsoid`: pick for eggs, pears, acorns, bullet heads, stylised \
  soft boxes. `ew`/`ns` = 1 is a sphere; `ew=1.3, ns=0.9` reads as \
  apple-like; `ew=2, ns=2` gives a rounded cube.
- `curved_plane`: petals, leaves, fish fins, roof tiles, shells. Unbent it \
  lies flat in XZ facing +Y; positive `bend_u`/`bend_v` curls the X/Z edges \
  toward +Y. For leaves, pair `bend_u` ≈ 30° with a small `bend_v` so the \
  leaf cups slightly along its length. **Single-sided — back face culled.** \
  If the underside can be seen (palm fronds, fins, any tilted leaf), set \
  `double_sided=1` on the leaf material. Do **not** wrap bent primitives in \
  `mirror (axis=y)` — mirror flips the Y-bend direction, so the two copies \
  diverge into a lens/saddle instead of sharing a surface.
- `lathe`: authored as a `(radius, y)` polyline, bottom row first. Use for \
  vases, gourds, onions, bulbs, fruits with a single axis of symmetry.
- `spline_tube`: the right primitive for bananas, stems, tentacles, horns, \
  elephant trunks, rope handles. `radii` (one per control point) tapers \
  the tube; a Catmull–Rom path keeps the curve smooth between points.
- `branch`: **recursive procedural tree** in one declaration. Emits a tapered \
  trunk + recursive forks (Catmull–Rom swept tubes) + optional alpha-cutout \
  leaf cards at the tips. Use for trees, large bushes, antlers, coral. Key \
  params: `length` (base trunk length), `radius` (base radius), `depth` \
  (recursion levels — 4–6 is typical for a tree), `splits` (forks per node, \
  2–3), `length_falloff`/`radius_falloff` (per-level multipliers, ~0.7/0.6), \
  `branch_angle` (deg off parent axis, ~30), `bend` (intra-segment curve, \
  ~10°), `tropism` (gravity droop; negative droops, positive lifts), \
  `seed` (regrow a different tree from the same params), `jitter` (0–1 \
  randomness on each fork), `leaves=1`, `leaf_size`, `leaf_mat=\"name\"`. \
  The wrapper accepts the usual `pos=`/`rot=`/`mat=` (bark goes on `mat=`).
- `leaf_card`: cross-quad / fan-quad foliage card. Two (or three) alpha- \
  cutout planes meeting at the +Y axis with their bottom edges at y=0, so \
  the `stem` connector mounts cleanly to a branch tip. Pair with a \
  `material (alpha_mode=\"mask\", alpha_cutoff=0.5, double_sided=1)` and \
  a leaf-shaped albedo texture for real foliage. `cards=2` (cross, default) \
  or `cards=3` (fan — defeats the edge-on-disappear artefact when the \
  camera circles).
- `torus`: flat in XZ, hole faces ±Y.
- `plane`, `disc`: flat in XZ, facing +Y. Single-sided like `curved_plane` — \
  set `double_sided=1` on the material if the underside is visible.
- `quad`: stands in XY, facing +Z (billboards, decals). Single-sided — set \
  `double_sided=1` on the material for signs/flags seen from both sides.

**Units are meters.** Use realistic scales so parts are legible at engine \
defaults:

| object     | typical size              |
|------------|---------------------------|
| humanoid   | ~1.7 m tall               |
| chair      | ~0.9 m tall, 0.5 m seat   |
| table      | ~0.75 m tall, 1.2 m wide  |
| door       | ~2.0 m tall, 0.9 m wide   |
| car        | ~4 m long, 1.5 m tall     |
| house room | ~3 m tall, 4 m wide       |
| tree       | ~3–8 m tall               |

Pick dimensions that match the object. Avoid unit cubes unless the prompt \
explicitly asks for an abstract block.";

const KINDS_REFERENCE: &str = "\
| kind | required attrs | notable attrs |
|------|----------------|----------------|
| `scene` | — | (container) |
| `group` | — | `pos`, `rot`, `scale`, `mat`, `role`, `tags` |
| `solid` | — | `mat`, `cleanup=\"coplanar\"\\|\"none\"` (default `none`); same-material leaf children merged at export |
| `stack` | — | `axis=x\\|y\\|z` (y), `gap` (0), `align=center\\|start\\|end`, `pack=start\\|center\\|end`; child layout by AABB slots |
| `grid` | — | `count=[x,y,z]` (1,1,1), `step=[x,y,z]` (0,0,0), `center=0\\|1`; N-dim lattice replicator |
| `material` | name | `color=[r,g,b]`, `alpha`, `metallic`, `roughness`, `alpha_mode=\"opaque\"\\|\"blend\"\\|\"mask\"`, `alpha_cutoff`, `emissive=[r,g,b]`, `emissive_strength` (HDR — use for neon/fluorescent), `transmission` (glass — use ALONE, never combined with `alpha`/`alpha_mode=\"blend\"` or the surface renders invisible; canonical glass is `transmission=0.9, roughness=0.05`), `double_sided=0\\|1` (disable back-face culling — leaves, fins, flags), `uv_mode=\"tile\"\\|\"fit\"` (default `tile` = world-space UVs for repeating textures; `fit` = per-face `[0,1]²` for sign/decal images), `uv_scale=N` or `[u,v]` (tiles per world unit in `tile` mode; default `1.0`) |
| `box` | `size=[x,y,z]` | `pos`, `rot`, `mat` |
| `rounded_box` | `size=[x,y,z]` | `radius`, `segments`, `pos`, `rot`, `mat` |
| `plane` | `size=[x,_,z]` | `pos`, `rot`, `mat` (XZ plane, +Y facing) |
| `quad` | `size=[x,y]` or `[x,y,_]` | `pos`, `rot`, `mat` (XY plane, +Z facing) |
| `disc` | `radius` | `segments`, `pos`, `rot`, `mat` |
| `cylinder` | `radius`, `height` | `segments`, `pos`, `rot`, `mat` |
| `cone` | `radius`, `height` | `segments`, `pos`, `rot`, `mat` |
| `capsule` | `radius`, `height` | `rings`, `segments`, `pos`, `rot`, `mat` |
| `sphere` | `radius` | `rings`, `segments`, `pos`, `rot`, `mat` |
| `icosphere` | `radius` | `subdivisions`, `pos`, `rot`, `mat` |
| `torus` | `major`, `minor` | `major_segments`, `minor_segments`, `pos`, `rot`, `mat` |
| `prism` | `size=[x,y,z]` | `pos`, `rot`, `mat` (symmetric isoceles, ridge along +Z) |
| `wedge` | `size=[x,y,z]` | doorstop: tall wall at -Z, slopes down toward +Z |
| `frustum` | `bottom=[x,z]`, `top=[x,z]`, `height` | tapered box; either end may be larger |
| `pyramid` | `radius`, `height` | `sides` (default 4), `pos`, `rot`, `mat` |
| `tube` | `outer`, `inner`, `height` | `segments` (24); hollow cylinder along Y |
| `hemisphere` | `radius` | `rings` (8), `segments` (24); flat base at y=0, dome at y=+r |
| `half_cylinder` | `radius`, `height` | `segments` (24); flat face on x=0, curves toward +X |
| `torus_arc` | `major`, `minor` | `arc` degrees (90), `*_segments`; partial torus around +Y |
| `ellipsoid` | `size=[x,y,z]` | `rings` (16), `segments` (24); size is diameter per axis |
| `superellipsoid` | `size=[x,y,z]` | `ew`, `ns` (1=sphere, >1 boxy, <1 pinched); eggs, pears, rounded boxes |
| `curved_plane` | `size=[x,z]` | `bend_u`, `bend_v` degrees; leaves, petals, shells, fins |
| `lathe` | `profile=[[r,y], …]` (bottom→top) | `segments`, `cap_ends`; vases, gourds, bulbs, onions |
| `spline_tube` | `points=[[x,y,z], …]` | `radius` or `radii=[…]`, `segments`, `samples`; bananas, stems, horns, handles |
| `leaf_card` | `size=[w,h]` | `cards` (2 cross / 3 fan); paired alpha-cutout planes for foliage — pair with `alpha_mode=\"mask\", double_sided=1` |
| `branch` | `length`, `radius`, `depth` | `splits`, `length_falloff`, `radius_falloff`, `branch_angle`, `roll`, `tropism`, `bend`, `seed`, `jitter`, `leaves`, `leaf_size`, `leaf_cards`, `leaf_mat`; **recursive procedural tree — one declaration becomes a whole tree** |
| `slab` | `size=[x,y,z]` | `box` alias; default `anchor=bottom` (sits on ground) |
| `post` | `size=[x,y,z]` | `box` alias; default `anchor=bottom` (pillar/leg/column) |
| `panel` | `size=[x,y,z]` | `box` alias; default `anchor=back` (wall-hung panel, flush to +Z face) |
| `wall` | `size=[x,y,z]` | `holes=[[cx, cy, w, h], …]`; rectangular CSG cutouts through Z — one watertight mesh, use for walls with doors/windows instead of nested `difference` |
| `connector` | name, `at=[...]` | `dir=[...]`, `tag=<ident>`, `radius` |
| `attach` | `parent`, `child` | `socket`, `plug` (default `top`/`bottom`), `offset`, `twist` |
| `mirror` | `axis=x|y|z` | children |
| `array` | `count`, `around=x|y|z` | `start_angle`, children |
| `module` | name, optional params | body |
| `use` | module name | args |
| `union`, `difference`, `intersect` | — | children are operands |
| `joint` | name, `type`, `pivot` | `axis`, `limits=[lo,hi]` |
| `clip` | name, `seconds` | `track` children |
| `track` | name (joint/node/bone) | `prop=\"translation\\|rotation\\|scale\"`, `axis=[x,y,z]`, `from`/`to` **or** `keys=[[t,v], ...]` |
| `skeleton` | name | contains `bone` children; accepts `pos`/`rot`/`scale` to place the rig |
| `bone` | name | `pos` (parent-relative offset), `rot`, `envelope` (default 0.75, meters); may nest `bone` children |
| `spin` | `target` | `axis`, `rpm` (60) |
| `open_close` | `target` | `axis`, `angle` (90°), `seconds` (1.0) |
| `wave` | `target` | `axis`, `amplitude` (15°), `hz` (1.0) |
| `flap` | `target` | `axis`, `amplitude` (30°), `hz` (2.0) |
| `idle` | `target` | `amplitude` (0.02), `hz` (0.5) — subtle breathing scale; no axis/angle |

### Default connectors per primitive (used by `attach`)

| primitive | connectors |
|-----------|------------|
| `box`, `rounded_box`, `prism` | `top`, `bottom`, `left`, `right`, `front`, `back` |
| `cylinder` | `top`, `bottom`, `side` (at +X on the wall) |
| `cone`, `pyramid` | `apex` / `top` (pointy end), `base` / `bottom` |
| `sphere`, `icosphere`, `ellipsoid`, `superellipsoid` | `top`, `bottom`, `left`, `right`, `front`, `back` |
| `curved_plane` | `top` (+Y), `bottom` (-Y) — unbent frame; bent geometry lifts off the origin |
| `lathe` | `top` (last profile row, +Y), `bottom` (first profile row, -Y) |
| `spline_tube` | `start` (first control point, -tangent), `end` (last control point, +tangent) |
| `leaf_card` | `stem` / `base` (origin, -Y mounting point), `tip` / `top` (top edge, +Y) |
| `capsule` | `top`, `bottom` (include the hemispherical caps) |
| `torus` | `top`, `bottom`, `outer`, `inner` |
| `plane`, `disc` | `top` (+Y), `bottom` (-Y) |
| `quad` | `front` (+Z), `back` (-Z) |
| `wedge` | `bottom`, `back`, `left`, `right`, `top` / `slope` (angled face faces +Y and +Z) |
| `frustum` | `top`, `bottom`, `left`, `right`, `front`, `back` (outer extents) |
| `tube` | `top`, `bottom`, `side` (at outer wall +X) |
| `hemisphere` | `top` / `apex` (+Y), `bottom` / `base` (flat face at y=0, facing -Y) |
| `half_cylinder` | `top`, `bottom`, `side` (+X curve peak), `flat` (x=0 face, -X) |
| `torus_arc` | `top`, `bottom`, `start` (cap at phi=0, -Z), `end` (cap at phi=arc) |";

const FEWSHOT: &str = "\
Ten prompt / output pairs spanning mechanical, architectural, and organic \
subjects. The user message will be a single short phrase like these.

### Prompt: \"a simple wooden stool\"
### Output:
material \"wood\" (color=[0.55, 0.35, 0.18], roughness=0.8)

scene {
  cylinder \"seat\" (radius=0.2, height=0.04, mat=\"wood\") {
    connector \"m_fl\" (at=[-0.13, -0.02, -0.13], dir=[0, -1, 0])
    connector \"m_fr\" (at=[ 0.13, -0.02, -0.13], dir=[0, -1, 0])
    connector \"m_bl\" (at=[-0.13, -0.02,  0.13], dir=[0, -1, 0])
    connector \"m_br\" (at=[ 0.13, -0.02,  0.13], dir=[0, -1, 0])
  }
  cylinder \"leg_fl\" (radius=0.025, height=0.45, mat=\"wood\")
  cylinder \"leg_fr\" (radius=0.025, height=0.45, mat=\"wood\")
  cylinder \"leg_bl\" (radius=0.025, height=0.45, mat=\"wood\")
  cylinder \"leg_br\" (radius=0.025, height=0.45, mat=\"wood\")
  attach (parent=\"seat\", child=\"leg_fl\", socket=\"m_fl\", plug=\"top\")
  attach (parent=\"seat\", child=\"leg_fr\", socket=\"m_fr\", plug=\"top\")
  attach (parent=\"seat\", child=\"leg_bl\", socket=\"m_bl\", plug=\"top\")
  attach (parent=\"seat\", child=\"leg_br\", socket=\"m_br\", plug=\"top\")
}

### Prompt: \"a snowman\"
### Output:
material \"snow\"   (color=[0.95, 0.96, 0.98], roughness=0.9)
material \"coal\"   (color=[0.08, 0.08, 0.08], roughness=0.5)
material \"carrot\" (color=[0.95, 0.45, 0.1],  roughness=0.7)

scene {
  sphere \"base\"  (radius=0.45, mat=\"snow\")
  sphere \"torso\" (radius=0.32, mat=\"snow\")
  sphere \"head\"  (radius=0.22, mat=\"snow\")
  cone   \"nose\"  (radius=0.03, height=0.12, mat=\"carrot\")
  sphere \"eye_l\" (radius=0.025, mat=\"coal\")
  sphere \"eye_r\" (radius=0.025, mat=\"coal\")

  attach (parent=\"base\",  child=\"torso\")
  attach (parent=\"torso\", child=\"head\")
  attach (parent=\"head\",  child=\"nose\",  socket=\"front\", plug=\"base\", twist=180)
  attach (parent=\"head\",  child=\"eye_l\", socket=\"front\", plug=\"back\", offset=-0.18, twist=-20)
  attach (parent=\"head\",  child=\"eye_r\", socket=\"front\", plug=\"back\", offset=-0.18, twist=20)
}

### Prompt: \"a spinning ceiling fan\"
### Output:
material \"metal\" (color=[0.7, 0.7, 0.72], metallic=0.9, roughness=0.3)
material \"wood\"  (color=[0.5, 0.32, 0.18], roughness=0.75)

scene {
  plane \"ceiling\" (pos=[0, 2.5, 0], size=[3, 0, 3], mat=\"wood\")
  cylinder \"stem\" (radius=0.03, height=0.25, mat=\"metal\")
  cylinder \"hub\"  (radius=0.12, height=0.06, mat=\"metal\")
  attach (parent=\"ceiling\", child=\"stem\", socket=\"bottom\", plug=\"top\")
  attach (parent=\"stem\",    child=\"hub\",  socket=\"bottom\", plug=\"top\")

  group \"blades\" (tags=\"floating\") {
    array \"bladeset\" (count=4, around=y) {
      box \"blade\" (pos=[0.45, 2.17, 0], size=[0.7, 0.02, 0.15], mat=\"wood\")
    }
  }
}

spin \"fan_spin\" (target=\"blades\", axis=[0, 1, 0], rpm=90)

### Prompt: \"a hollow crate with a swinging lid\"
### Output:
material \"wood\" (color=[0.45, 0.28, 0.15], roughness=0.8)

scene {
  difference \"crate\" {
    box \"outer\"  (size=[0.8, 0.6, 0.8], mat=\"wood\")
    box \"hollow\" (pos=[0, 0.05, 0], size=[0.7, 0.55, 0.7])
  }
  // The lid swings from its back edge. Place the hinge group's origin
  // AT that edge and offset the lid mesh forward inside, so rotating
  // the group pivots the lid about the edge — not its centre.
  group \"lid_hinge\" (pos=[0, 0.325, -0.4]) {
    box \"lid\" (pos=[0, 0, 0.4], size=[0.8, 0.05, 0.8], mat=\"wood\")
  }
}

joint \"lid_pivot\" (type=hinge, axis=[1, 0, 0], pivot=\"lid_hinge\")
open_close \"lid_swing\" (target=\"lid_pivot\", angle=85, seconds=0.8)

### Prompt: \"a stone archway\"
### Output:
material \"stone\" (color=[0.55, 0.52, 0.48], roughness=0.9)

scene {
  solid \"arch\" (mat=\"stone\", cleanup=\"coplanar\") {
    post \"left\"   (x=-0.9, size=[0.3, 2.4, 0.3])
    post \"right\"  (x= 0.9, size=[0.3, 2.4, 0.3])
    slab \"lintel\" (above=\"left\", x=0, size=[2.1, 0.3, 0.3])
  }
}

### Prompt: \"a potted fern\"
### Output:
material \"pot\"  (color=[0.55, 0.32, 0.22], roughness=0.85)
material \"leaf\" (color=[0.2, 0.55, 0.22],  roughness=0.6, double_sided=1)

scene {
  frustum \"pot\" (bottom=[0.18, 0.18], top=[0.22, 0.22], height=0.2, mat=\"pot\")
  sphere  \"hub\" (radius=0.04, mat=\"leaf\") {
    array \"fronds\" (count=6, around=y) {
      group \"place\" (pos=[0, 0, 0.35], rot=[25, 0, 0]) {
        curved_plane \"frond\" (size=[0.3, 0.7], bend_u=30, bend_v=-30, mat=\"leaf\")
      }
    }
  }
  attach (parent=\"pot\", child=\"hub\", socket=\"top\", plug=\"bottom\")
}

### Prompt: \"a person walking\"
### Output:
material \"skin_m\" (color=[0.82, 0.64, 0.55], roughness=0.7)
material \"shirt\"  (color=[0.22, 0.38, 0.62], roughness=0.75)
material \"pants\"  (color=[0.15, 0.16, 0.2],  roughness=0.8)

scene {
  skeleton \"rig\" {
    bone \"hip\" (pos=[0, 0.95, 0], envelope=0.28) {
      bone \"spine\" (pos=[0, 0.3, 0], envelope=0.32) {
        bone \"neck\" (pos=[0, 0.2, 0], envelope=0.15)
        bone \"shoulder_l\" (pos=[ 0.2, 0.18, 0], envelope=0.2) {
          bone \"elbow_l\" (pos=[0, -0.25, 0], envelope=0.15)
        }
        bone \"shoulder_r\" (pos=[-0.2, 0.18, 0], envelope=0.2) {
          bone \"elbow_r\" (pos=[0, -0.25, 0], envelope=0.15)
        }
      }
      bone \"hip_l\" (pos=[ 0.1, 0, 0], envelope=0.22) {
        bone \"knee_l\" (pos=[0, -0.45, 0], envelope=0.2)
      }
      bone \"hip_r\" (pos=[-0.1, 0, 0], envelope=0.22) {
        bone \"knee_r\" (pos=[0, -0.45, 0], envelope=0.2)
      }
    }
  }

  sphere   \"head\"  (pos=[0, 1.6, 0],  radius=0.12, mat=\"skin_m\", skin=\"rig\")
  cylinder \"torso\" (pos=[0, 1.2, 0],  radius=0.18, height=0.55, mat=\"shirt\", skin=\"rig\")
  cylinder \"arm_l\" (pos=[ 0.27, 1.1, 0], radius=0.06, height=0.5, mat=\"skin_m\", skin=\"rig\")
  cylinder \"arm_r\" (pos=[-0.27, 1.1, 0], radius=0.06, height=0.5, mat=\"skin_m\", skin=\"rig\")
  cylinder \"leg_l\" (pos=[ 0.1, 0.5, 0], radius=0.08, height=0.9, mat=\"pants\",  skin=\"rig\")
  cylinder \"leg_r\" (pos=[-0.1, 0.5, 0], radius=0.08, height=0.9, mat=\"pants\",  skin=\"rig\")
}

clip \"walk\" (seconds=1.0) {
  track \"hip_l\"      (prop=rotation, axis=[1, 0, 0], keys=[[0, -25], [0.5,  25], [1.0, -25]])
  track \"hip_r\"      (prop=rotation, axis=[1, 0, 0], keys=[[0,  25], [0.5, -25], [1.0,  25]])
  track \"shoulder_l\" (prop=rotation, axis=[1, 0, 0], keys=[[0,  20], [0.5, -20], [1.0,  20]])
  track \"shoulder_r\" (prop=rotation, axis=[1, 0, 0], keys=[[0, -20], [0.5,  20], [1.0, -20]])
}

### Prompt: \"a crouching tiger\"
### Output:
material \"tiger_back_fur\"  (color=[0.85, 0.45, 0.15], roughness=0.85)
material \"tiger_belly_fur\" (color=[0.96, 0.92, 0.85], roughness=0.85)

scene {
  group \"body\" (y=0.4, mat=\"tiger_back_fur\") {
    use \"quadruped_torso\" (length=0.95, height=0.32, width=0.32)
  }
  group \"head\" (pos=[0, 0.5, 0.55], mat=\"tiger_back_fur\") {
    use \"humanoid_head\" (size=0.13, jaw=0.5)
  }
  capsule \"leg_fl\" (pos=[ 0.13, 0.16,  0.32], radius=0.05, height=0.32, mat=\"tiger_back_fur\")
  capsule \"leg_fr\" (pos=[-0.13, 0.16,  0.32], radius=0.05, height=0.32, mat=\"tiger_back_fur\")
  capsule \"leg_bl\" (pos=[ 0.13, 0.18, -0.32], radius=0.05, height=0.28, mat=\"tiger_back_fur\")
  capsule \"leg_br\" (pos=[-0.13, 0.18, -0.32], radius=0.05, height=0.28, mat=\"tiger_back_fur\")
  group \"tail\" (pos=[0, 0.45, -0.45], mat=\"tiger_back_fur\") { use \"tail\" () }
}

### Prompt: \"a person mid-stride\"
### Output:
material \"skin\"  (color=[0.92, 0.78, 0.65], roughness=0.7)
material \"shirt\" (color=[0.20, 0.45, 0.75], roughness=0.85)
material \"pants\" (color=[0.18, 0.20, 0.25], roughness=0.85)

scene {
  group \"body\" (y=1.05, mat=\"shirt\") {
    use \"humanoid_torso\" (height=0.55, width=0.36, depth=0.22)
  }
  group \"head\" (pos=[0, 1.4, 0], mat=\"skin\") {
    use \"humanoid_head\" (size=0.11, jaw=0.55)
  }
  group \"leg_r\" (pos=[-0.1, 1.0,  0.05], rot=[ 20, 0, 0], mat=\"pants\") { use \"humanoid_leg\" (length=0.9, radius=0.07) }
  group \"leg_l\" (pos=[ 0.1, 1.0, -0.05], rot=[-20, 0, 0], mat=\"pants\") { use \"humanoid_leg\" (length=0.9, radius=0.07) }
  group \"arm_l\" (pos=[ 0.22, 1.45, -0.05], rot=[ 15, 0, 0], mat=\"skin\") { use \"humanoid_arm\" (length=0.55, radius=0.05) }
  group \"arm_r\" (pos=[-0.22, 1.45,  0.05], rot=[-15, 0, 0], mat=\"skin\") { use \"humanoid_arm\" (length=0.55, radius=0.05) }
}

### Prompt: \"a young oak tree\"
### Output:
material \"oak_bark\" (color=[0.36, 0.25, 0.15], roughness=0.95)
material \"oak_leaf\" (
  color=[0.20, 0.50, 0.22], roughness=0.65,
  alpha_mode=\"mask\", alpha_cutoff=0.5, double_sided=1
)

scene {
  branch \"oak\" (
    length=1.4, radius=0.18, depth=5, splits=2,
    length_falloff=0.72, radius_falloff=0.62,
    branch_angle=32, bend=12, tropism=-0.05, jitter=0.25, seed=7,
    leaves=1, leaf_size=0.32, leaf_cards=2, leaf_mat=\"oak_leaf\",
    mat=\"oak_bark\"
  )
}";

const OUTPUT_CONTRACT: &str = "\n\n## Output contract\n\n\
Reply with ONLY the DSL source. No prose, no backticks, no language tag, no \
leading or trailing explanations.

Before you emit, silently verify:

1. Every `mat=\"X\"` has a matching `material \"X\" (...)` somewhere in the file.
2. Every `attach parent=`/`child=`, `joint pivot=`, and animation `target=` \
   names a node that actually exists.
3. Every primitive is joined to the rest by `attach` (directly or \
   transitively) OR carries `tags=\"floating\"` on itself or an ancestor.
4. No `pos=` appears on a part that should be joined to another part — \
   those joins use `attach`.
5. No `attach` joins two parts that are supposed to share a centre \
   (pane in frame, liquid in glass, core in shell) — those sit at origin.
6. No `material` sets `transmission` AND `alpha`/`alpha_mode=\"blend\"` \
   together — pick transmission for glass, alpha for tints/gels, not both.
7. The scene stays compact — a handful of primitives or a small number of \
   modules, not hundreds of shapes.
8. Every `skin=\"X\"` names a declared `skeleton \"X\" { ... }` in the same \
   file. Rigged scenes have all limb cycles inside one `clip` with \
   coordinated `keys=` tracks, not a scatter of one-clip-per-limb \
   procedural templates.
9. Flush-joined siblings use `above=\"sib\"` / `below=\"sib\"` / \
   `left_of=\"sib\"` / `right_of=\"sib\"` / `in_front_of=\"sib\"` / \
   `behind=\"sib\"` (+ `gap=`), or a `stack`/`grid` wrapper — not \
   hand-computed `pos=`. Reach for `slab`/`post`/`panel` (box aliases \
   with `anchor=bottom`/`back` defaults) and `anchor=…` before layering \
   on `pos=`.
10. When many primitives of one material form a single solid shape \
    (stone walls of a hut, an archway, a tower), wrap them in \
    `solid { ... }` so interior faces vanish; add `cleanup=\"coplanar\"` \
    when boxes merely *touch* at perpendicular seams. Use `wall \
    (holes=[[cx,cy,w,h], …])` for walls with doors/windows rather than \
    nested `difference { box box box }`.

If the prompt is ambiguous, make a reasonable choice and commit to it.\n";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_index_renders_placeholder() {
        let s = system_instruction(&StdlibIndex::default());
        assert!(s.contains("No stdlib modules"));
        assert!(s.contains("## DSL grammar"));
        assert!(s.contains("## Output contract"));
        assert!(s.contains("## Conventions"));
        assert!(s.contains("## Prompt → DSL demonstrations"));
    }

    #[test]
    fn populated_index_lists_modules_with_defaults() {
        let mut idx = StdlibIndex::default();
        idx.modules.push(ModuleSummary {
            name: "leg".into(),
            params: vec![("height".into(), Some("0.5".into())), ("radius".into(), Some("0.05".into()))],
            doc: Some("a cylindrical leg".into()),
        });
        idx.modules.push(ModuleSummary {
            name: "slab".into(),
            params: vec![("width".into(), Some("1".into())), ("required".into(), None)],
            doc: None,
        });
        let s = system_instruction(&idx);
        assert!(s.contains("`leg(height=0.5, radius=0.05)`"));
        assert!(s.contains("a cylindrical leg"));
        assert!(s.contains("`slab(width=1, required)`"));
        // Populated index swaps the placeholder for the "prefer use" nudge.
        assert!(s.contains("prefer `use"));
        assert!(!s.contains("No stdlib modules"));
    }

    #[test]
    fn output_is_byte_stable_for_same_input() {
        // Cache-key property: repeated calls produce identical bytes.
        let idx = StdlibIndex::default();
        let a = system_instruction(&idx);
        let b = system_instruction(&idx);
        assert_eq!(a, b);
    }

    #[test]
    fn from_registry_sorts_module_names() {
        let ast = mogen_dsl::parse(
            r#"
            module "zed" (x=1) { box "b" (size=[1,1,1]) }
            module "alpha" (y=2) { box "b" (size=[1,1,1]) }
            "#,
        )
        .unwrap();
        let reg = mogen_dsl::collect_modules(&ast).unwrap();
        let idx = StdlibIndex::from_registry(&reg);
        assert_eq!(idx.modules.len(), 2);
        assert_eq!(idx.modules[0].name, "alpha");
        assert_eq!(idx.modules[1].name, "zed");
    }

    #[test]
    fn kinds_table_lists_every_primitive() {
        // If a primitive exists in mogen-geom but not here, the model won't use it.
        let s = system_instruction(&StdlibIndex::default());
        for kind in [
            "box", "rounded_box", "plane", "quad", "disc", "cylinder", "cone",
            "capsule", "sphere", "icosphere", "torus", "prism", "pyramid",
            "wedge", "frustum", "tube", "hemisphere", "half_cylinder",
            "torus_arc", "ellipsoid", "superellipsoid", "curved_plane",
            "lathe", "spline_tube",
            // Foliage card + recursive procedural tree.
            "leaf_card", "branch",
            // Box aliases + hole-punched wall.
            "slab", "post", "panel", "wall",
        ] {
            assert!(s.contains(&format!("`{kind}`")), "kinds table missing {kind}");
        }
    }

    #[test]
    fn exposes_solid_stack_grid_and_placement_shortcuts() {
        // These features all existed in the DSL but weren't being pushed by
        // the system instruction — so the LLM never reached for them.
        let s = system_instruction(&StdlibIndex::default());
        // Containers:
        assert!(s.contains("`solid`"), "kinds table missing solid");
        assert!(s.contains("`stack`"), "kinds table missing stack");
        assert!(s.contains("`grid`"), "kinds table missing grid");
        assert!(
            s.contains("cleanup=\"coplanar\""),
            "grammar reference missing solid cleanup option"
        );
        // Placement shortcuts:
        assert!(s.contains("`anchor=bottom"), "grammar missing anchor shortcut");
        assert!(s.contains("above=\"sib\""), "grammar missing relative placement");
        assert!(s.contains("`from=[x,y,z]"), "grammar missing from/to corners");
        // Output-contract self-check picks them up:
        assert!(s.contains("Flush-joined siblings"), "output contract missing sibling-placement rule");
        assert!(s.contains("single solid shape"), "output contract missing solid-grouping rule");
        // An archway fewshot actually demonstrates solid+post+slab+above+cleanup.
        assert!(
            s.contains("Prompt: \"a stone archway\""),
            "stone archway fewshot missing"
        );
        assert!(
            s.contains("solid \"arch\" (mat=\"stone\", cleanup=\"coplanar\")"),
            "archway fewshot should use solid + cleanup"
        );
    }

    #[test]
    fn includes_numbered_rules_and_conventions() {
        let s = system_instruction(&StdlibIndex::default());
        // Load-bearing rules are numbered at the top.
        assert!(s.contains("1. Root the scene"));
        assert!(s.contains("2. Join parts with `attach`"));
        // Units and orientation are stated explicitly.
        assert!(s.contains("Units are meters"));
        assert!(s.contains("canonical orientation"));
    }

    #[test]
    fn output_contract_has_self_check() {
        let s = system_instruction(&StdlibIndex::default());
        assert!(s.contains("Before you emit, silently verify"));
    }

    #[test]
    fn exposes_rigging_and_multi_keyframe_tracks() {
        // The skinning path: the model won't emit rigs unless it sees them
        // in the grammar reference, the kinds table, and a fewshot.
        let s = system_instruction(&StdlibIndex::default());
        assert!(s.contains("`skeleton`"), "kinds table missing skeleton row");
        assert!(s.contains("`bone`"), "kinds table missing bone row");
        assert!(s.contains("skin=\"rig\""), "grammar reference missing skin= example");
        assert!(s.contains("parent-relative"), "conventions missing bone-pos-is-parent-relative rule");
        assert!(s.contains("envelope"), "conventions missing envelope guidance");
        // Multi-keyframe tracks:
        assert!(s.contains("keys=[[t, v]"), "grammar reference missing keys=[[t,v]] form");
        // The humanoid fewshot is the concrete demonstration.
        assert!(
            s.contains("Prompt: \"a person walking\""),
            "humanoid walk fewshot missing"
        );
        assert!(
            s.contains("clip \"walk\" (seconds=1.0)"),
            "humanoid walk clip missing"
        );
    }

    #[test]
    fn is_materially_shorter_than_legacy() {
        // Guard against regressions. Pre-consolidation (separate EXAMPLES and
        // ANTI_PATTERN sections) the assembly was ~15 KB with an empty stdlib
        // index. Organic primitives (superellipsoid, curved_plane, lathe,
        // spline_tube) added ~1.4 KB across the kinds table, connectors
        // table, and conventions block. A single-sidedness rule for
        // curved_plane/plane/disc/quad plus a potted-fern fewshot showing the
        // group+mirror pattern added ~0.9 KB; steering the model onto the
        // `double_sided=1` material flag (and away from mirroring bent
        // planes, which produced divergent sheets rather than double-sided
        // leaves) added another ~0.1 KB; teaching the model about CSG
        // material inheritance, cut-through geometry, and coplanar-face
        // avoidance added ~0.4 KB; steering the model away from using
        // `attach` on concentric parts (pane-in-frame, core-in-shell) and
        // from stacking `transmission` with `alpha_mode="blend"` on glass
        // added ~0.75 KB. Exposing skinning (skeleton/bone/skin=, multi-
        // keyframe `keys=` tracks, and a humanoid walk fewshot) added ~4 KB;
        // teaching the model about world-space `uv_mode="tile"` vs
        // image-style `uv_mode="fit"` and `uv_scale` density tuning added
        // ~0.6 KB. Surfacing placement shortcuts (`x=/y=/z=/rx=/.../w=/h=/d=/
        // from=/to=/anchor=/above=/below=/left_of=…`), the `solid`/`stack`/
        // `grid` containers, the `slab`/`post`/`panel`/`wall` aliases, plus
        // a stone-archway fewshot and two new output-contract rules added
        // ~3.7 KB — without them the model would never reach for any of them.
        // Emitting the validator's `attrs_for_kind` table as an authoritative
        // closed allowlist (so the model stops hallucinating attrs like
        // `from=` on `open_close`) plus the intro paragraph that re-states
        // which kinds accept common attrs added ~3.5 KB. Three organic
        // fewshots (tiger / humanoid mid-stride / oak tree) and the new
        // organic-shapes preamble rule added ~3.6 KB — they're what makes
        // the LLM reach for the body-part stdlib instead of rebuilding
        // limbs from raw primitives. Adding the recursive `branch` node +
        // `leaf_card` primitive (kinds table, conventions paragraphs,
        // connectors row, organic-shapes preamble update) was net-neutral:
        // the new prose added ~1.4 KB but rewriting the oak-tree fewshot
        // from a hand-stacked 18-line layout into a single procedural
        // `branch (...)` declaration shaved ~1.1 KB back off. The cap now
        // sits at ~36 KB — still small enough to catch a revived long-form
        // section.
        let s = system_instruction(&StdlibIndex::default());
        assert!(
            s.len() < 36_000,
            "system instruction grew to {} bytes — did a section come back?",
            s.len()
        );
    }

    #[test]
    fn exposes_closed_attribute_allowlist() {
        // The allowlist section exists, calls itself authoritative, and renders
        // the per-kind rows verbatim from the validator so prompt and validator
        // can't drift apart.
        let s = system_instruction(&StdlibIndex::default());
        assert!(s.contains("## Attribute allowlist"));
        assert!(s.contains("closed list of kind-specific attributes"));
        // A sampling of kinds whose exact attrs the model must respect.
        assert!(s.contains("- `open_close`: `target`, `axis`, `angle`, `seconds`"));
        assert!(s.contains("- `joint`: `type`, `axis`, `limits`, `pivot`"));
        assert!(s.contains("- `attach`: `parent`, `child`, `socket`, `plug`, `offset`, `twist`"));
        // Empty-allowlist kinds still appear so the model sees they take no
        // kind-specific attrs (only common ones).
        assert!(s.contains("- `scene`: (no kind-specific attrs)"));
    }

    #[test]
    fn exposes_recursive_branch_and_leaf_card() {
        // The procedural-tree path: kinds table, conventions paragraph,
        // attribute allowlist, organic-shapes preamble, and the oak-tree
        // fewshot all need to push `branch` so the model reaches for it
        // instead of stacking `use \"branch\"` modules manually.
        let s = system_instruction(&StdlibIndex::default());
        // Kinds table rows.
        assert!(s.contains("`branch`"), "kinds table missing branch row");
        assert!(s.contains("`leaf_card`"), "kinds table missing leaf_card row");
        // Conventions paragraph names the recursive-tree feature.
        assert!(
            s.contains("recursive procedural tree"),
            "conventions missing recursive procedural tree blurb"
        );
        // Allowlist auto-renders; spot-check a few critical attrs.
        assert!(
            s.contains("`branch_angle`"),
            "allowlist missing branch_angle for branch"
        );
        assert!(
            s.contains("`leaf_mat`"),
            "allowlist missing leaf_mat for branch"
        );
        // Default connectors row for leaf_card so attach math knows where
        // a leaf mounts on a branch tip.
        assert!(
            s.contains("`stem`"),
            "connectors table missing leaf_card stem"
        );
        // Oak fewshot uses the procedural branch node now.
        assert!(
            s.contains("branch \"oak\""),
            "oak fewshot should use procedural branch"
        );
        // The leaf material in the fewshot demonstrates alpha-mask + double-sided.
        assert!(
            s.contains("alpha_mode=\"mask\""),
            "oak fewshot missing alpha_mode=mask on leaf material"
        );
    }

    #[test]
    fn crate_fewshot_uses_hinge_group_not_mesh_center() {
        // Regression: the earlier fewshot attached `open_close` to the lid
        // mesh directly, which rotates about its centre. The canonical pattern
        // is joint + pivot on a group whose origin sits at the hinge edge.
        let s = system_instruction(&StdlibIndex::default());
        assert!(s.contains("group \"lid_hinge\""));
        assert!(s.contains("joint \"lid_pivot\" (type=hinge, axis=[1, 0, 0], pivot=\"lid_hinge\")"));
        assert!(s.contains("open_close \"lid_swing\" (target=\"lid_pivot\""));
    }
}
