#!/usr/bin/env python3
"""Generate `gatehouse.pascal.json` — our own scene, in pascalorg/editor's format.

Run from the repo root:

    python3 crates/mogen-pascal/tests/fixtures/make_gatehouse.py

This exists so the importer's main regression case is a building *we* designed,
rather than someone's project off the community gallery. Keeping the generator
rather than only its output means the awkward parts below are declared on
purpose and can be adjusted, instead of being mystery bytes in a fixture.

The plan is a T: a 14x8 main block with a 6x5 wing off its north face, two
storeys plus a roof level, cross-gabled so the wing gets its own ridge.

Four things here are deliberately awkward, because each is a shape the importer
has to survive and none of them arises from a tidy hand-written scene:

1. The `site` node stores `polygon` as `{type, points}` rather than a bare ring.
   That wrapper only appears in scenes taken from the running app; their file
   exporter flattens it. It used to abort the whole load.
2. The `roof` container sits at a non-zero offset, and its segments are
   positioned relative to it. Reading only the segments leaves the roof
   floating off the walls.
3. Levels are numbered 0, 3, 5 rather than 0, 1, 2, and carry no `height`.
   Their level ordinals are sparse by design, and elevation comes from the
   prefix sum of the levels below, not from the ordinal.
4. Three shapes are genuinely malformed: a self-intersecting ring, a hole that
   hangs outside its slab, and a zero-length wall. Their editor tolerates all
   three; we must report and drop them rather than emit a hole. A test asserts
   these are still reported, so the day one silently starts importing is the
   day that test goes red.
"""

import json
import os

nodes = {}


def add(id, type, parent=None, **kw):
    n = {"id": id, "type": type, "object": "node", "visible": True,
         "parentId": parent, "children": []}
    n.update(kw)
    nodes[id] = n
    if parent:
        nodes[parent]["children"].append(id)
    return id


# -- shell ------------------------------------------------------------------
add("site_0", "site", None, name="Site",
    # (1) the wrapped form, not a bare ring.
    polygon={"type": "polygon",
             "points": [[-20, -20], [20, -20], [20, 20], [-20, 20]]})
add("bld_0", "building", None, name="Gatehouse")

# (3) sparse ordinals, no heights.
GROUND, UPPER, ROOF = "lvl_g", "lvl_u", "lvl_r"
add(GROUND, "level", "bld_0", name="Ground", level=0)
add(UPPER, "level", "bld_0", name="Upper", level=3)
add(ROOF, "level", "bld_0", name="Roof", level=5)

# -- plan -------------------------------------------------------------------
# Main block corners, then the wing off the north (+z) face.
X0, X1, Z0, Z1 = -7.0, 7.0, -4.0, 4.0
WX0, WX1, WZ = -3.0, 3.0, 9.0
WT_EXT, WT_INT = 0.3, 0.15

PERIMETER = [
    ((X0, Z0), (X1, Z0)),      # south
    ((X1, Z0), (X1, Z1)),      # east
    ((X1, Z1), (WX1, Z1)),     # north, east of the wing
    ((WX1, Z1), (WX1, WZ)),    # wing east
    ((WX1, WZ), (WX0, WZ)),    # wing north
    ((WX0, WZ), (WX0, Z1)),    # wing west
    ((WX0, Z1), (X0, Z1)),     # north, west of the wing
    ((X0, Z1), (X0, Z0)),      # west
]

# A central corridor with rooms either side, and the wing split in two.
GROUND_INT = [
    ((-2.0, Z0), (-2.0, Z1)),
    ((2.0, Z0), (2.0, Z1)),
    ((X0, 0.5), (-2.0, 0.5)),
    ((2.0, 0.5), (X1, 0.5)),
    ((WX0, 6.5), (WX1, 6.5)),
]
# Upper storey partitions the main block differently: four bedrooms, no
# corridor, so the two levels are not copies of each other.
UPPER_INT = [
    ((0.0, Z0), (0.0, Z1)),
    ((X0, 0.0), (X1, 0.0)),
    ((WX0, 6.0), (WX1, 6.0)),
]

wall_n = 0


def wall(level, a, b, thickness, height=None, openings=()):
    global wall_n
    wid = f"wall_{wall_n}"
    wall_n += 1
    kw = {"start": list(a), "end": list(b), "thickness": thickness}
    if height is not None:
        kw["height"] = height
    add(wid, "wall", level, **kw)
    for kind, along, centre, w, h in openings:
        add(f"{wid}_{kind}_{along}", kind, wid,
            position=[along, centre, 0], width=w, height=h,
            openingKind=kind)
    return wid


def length(a, b):
    return ((b[0] - a[0]) ** 2 + (b[1] - a[1]) ** 2) ** 0.5


for level, interior, sill in ((GROUND, GROUND_INT, 1.1), (UPPER, UPPER_INT, 1.2)):
    for i, (a, b) in enumerate(PERIMETER):
        L = length(a, b)
        holes = []
        # A door on the south wall at ground level; windows elsewhere, spaced
        # along whatever the wall's own length happens to be.
        if level == GROUND and i == 0:
            holes.append(("door", round(L * 0.5, 3), 1.05, 1.0, 2.1))
        n_win = max(1, int(L // 3.5))
        for k in range(n_win):
            at = round(L * (k + 0.5) / n_win, 3)
            if any(abs(at - h[1]) < 1.6 for h in holes):
                continue
            holes.append(("window", at, sill + 0.6, 1.2, 1.2))
        wall(level, a, b, WT_EXT, openings=holes)
    for a, b in interior:
        L = length(a, b)
        wall(level, a, b, WT_INT,
             openings=[("door", round(L * 0.5, 3), 1.05, 0.9, 2.0)])

# (4a) a zero-length wall, and one whose curve is tighter than its own
# half-thickness -- its footprint crosses itself, which the triangulator would
# accept and quietly return without caps.
wall(UPPER, (X1, Z1), (X1, Z1), WT_INT)
# A 1 m chord bowed 0.5 gives a radius of 0.5, inside the wall's own
# half-thickness of 0.6, so the inner offset turns inside out.
add(f"wall_{wall_n}", "wall", UPPER, start=[-6.0, 2.4], end=[-5.0, 2.4],
    thickness=1.2, curveOffset=0.5)
wall_n += 1

# -- floors -----------------------------------------------------------------
FOOTPRINT = [[X0, Z0], [X1, Z0], [X1, Z1], [WX1, Z1], [WX1, WZ],
             [WX0, WZ], [WX0, Z1], [X0, Z1]]
STAIR_HOLE = [[0.6, 1.0], [2.4, 1.0], [2.4, 3.4], [0.6, 3.4]]

add("slab_g", "slab", GROUND, name="Ground Slab",
    polygon=FOOTPRINT, holes=[], elevation=0.05, thickness=0.2)
add("slab_u", "slab", UPPER, name="Upper Slab",
    polygon=FOOTPRINT, holes=[STAIR_HOLE], elevation=0.05, thickness=0.2)
add("ceil_g", "ceiling", GROUND, name="Ground Ceiling",
    polygon=FOOTPRINT, holes=[STAIR_HOLE], elevation=0.0, thickness=0.05)
add("ceil_u", "ceiling", UPPER, name="Upper Ceiling",
    polygon=FOOTPRINT, holes=[], elevation=0.0, thickness=0.05)

# (4b) a hole that hangs outside its outer ring, the way an auto-generated
# stair opening does when the room it was cut from has since been resized.
add("slab_bad_hole", "slab", UPPER, name="Landing",
    polygon=[[WX0, 5.0], [WX1, 5.0], [WX1, 7.0], [WX0, 7.0]],
    holes=[[[2.0, 6.0], [5.0, 6.0], [5.0, 8.5], [2.0, 8.5]]],
    elevation=0.05, thickness=0.2)

# (4c) a self-intersecting ring -- a bowtie.
add("ceil_bad_ring", "ceiling", UPPER, name="Auto Ceiling",
    polygon=[[-2.0, -2.0], [2.0, 2.0], [2.0, -2.0], [-2.0, 2.0]],
    holes=[], elevation=0.0, thickness=0.05)

# -- roof -------------------------------------------------------------------
# (2) the container carries a transform and the segments are relative to it.
add("roof_0", "roof", ROOF, name="Roof", position=[0.4, -1.2, 0.6], rotation=0)
# Main ridge runs east-west, so the segment is turned a quarter turn; the wing
# gets its own gable across it.
add("rseg_main", "roof-segment", "roof_0",
    position=[-0.4, 0, -0.6], rotation=1.5707963267948966,
    roofType="gable", width=8.6, depth=14.6, pitch=42.0,
    overhang=0.3, wallHeight=0.4)
add("rseg_wing", "roof-segment", "roof_0",
    position=[-0.4, 0, 5.4], rotation=0,
    roofType="gable", width=6.6, depth=5.6, pitch=42.0,
    overhang=0.3, wallHeight=1.2)

# -- furniture --------------------------------------------------------------
FURNITURE = [
    ("Sofa", "seating", [-4.5, 0, -2.0], 0.0),
    ("Armchair", "seating", [-5.5, 0, 1.6], 1.57),
    ("Dining Table", "table", [4.5, 0, -2.0], 0.0),
    ("Kitchen Counter", "storage", [4.5, 0, 2.2], 3.14),
    ("Wood Stove", "appliance", [0.0, 0, -3.2], 0.0),
    ("Workbench", "table", [0.0, 0, 7.8], 0.0),
    ("Shelving", "storage", [-2.6, 0, 5.4], 1.57),
]
for i, (name, cat, pos, rot) in enumerate(FURNITURE):
    add(f"item_{i}", "item", GROUND, name=name, position=pos,
        rotation=[0, rot, 0],
        asset={"id": f"mogen/{cat}", "category": cat,
               "dimensions": [1.0, 0.8, 0.6]})
for i, (name, cat, pos, rot) in enumerate([
    ("Bed", "bed", [-4.0, 0, -2.0], 0.0),
    ("Bed", "bed", [4.0, 0, -2.0], 0.0),
    ("Wardrobe", "storage", [-4.0, 0, 2.6], 3.14),
    ("Desk", "table", [4.0, 0, 2.6], 3.14),
    ("Cot", "bed", [0.0, 0, 7.6], 0.0),
]):
    add(f"item_u{i}", "item", UPPER, name=name, position=pos,
        rotation=[0, rot, 0],
        asset={"id": f"mogen/{cat}", "category": cat,
               "dimensions": [1.0, 0.8, 0.6]})

# A kind we do not model, to keep the "reported, never fatal" path exercised.
add("guide_0", "guide", GROUND, name="Setting Out Line",
    start=[X0, Z0], end=[X1, Z1])

out = {"nodes": nodes, "rootNodeIds": ["site_0", "bld_0"]}
root = os.path.abspath(
    os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", "..", "..", "..")
)
path = os.path.join(root, "examples", "buildings", "gatehouse.pascal.json")
with open(path, "w") as f:
    json.dump(out, f, separators=(",", ":"), sort_keys=True)
print(f"{path}: {len(nodes)} nodes, {wall_n} walls")
