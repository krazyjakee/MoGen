const e=`// A single open-interior room built from overlapping wall boxes. The
// \`solid { ... }\` wrapper CSG-unions its same-material leaf children at
// export time, removing the redundant interior faces where the walls meet.
// \`cleanup="coplanar"\` additionally drops pairs of triangles that share a
// plane with opposite normals — the artefact CSG union leaves behind when
// two boxes touch along a face without overlapping (e.g. perpendicular
// walls meeting at a corner).
//
// Compare this to \`simple_house.mog\`, which uses \`difference\` to cut openings
// in each wall separately. \`solid\` is for the opposite case: you have many
// pieces of the same material that should read as one hollow shape.

material "stone" (color=[0.82, 0.78, 0.72], metallic=0.0, roughness=0.85)
material "glass" (color=[0.70, 0.85, 0.95], metallic=0.0, roughness=0.10)

scene {
  solid "shell" (mat="stone", role="building", cleanup="coplanar") {
    // Floor slab — extends slightly past the walls so corner seams union cleanly.
    box "floor"  (pos=[0,  0.1, 0], size=[6.2, 0.2, 4.2])

    // Four walls. They overlap the floor and each other at the corners on
    // purpose; the solid/CSG pass removes interior faces produced by those
    // overlaps, and coplanar cleanup collapses the touching corner seams.
    box "north"  (pos=[0,  1.7,  2.0], size=[6.0, 3.0, 0.2])
    box "south"  (pos=[0,  1.7, -2.0], size=[6.0, 3.0, 0.2])
    box "east"   (pos=[ 3.0, 1.7, 0],  size=[0.2, 3.0, 4.0])
    box "west"   (pos=[-3.0, 1.7, 0],  size=[0.2, 3.0, 4.0])

    // Gable-less flat roof, same material so it also merges in.
    box "ceiling" (pos=[0, 3.3, 0], size=[6.2, 0.2, 4.2])
  }

  // A window pane is a different material, so it stays a separate node —
  // it sits inside the room looking out through an imagined opening.
  box "window_pane" (pos=[0, 1.8, 1.9], size=[1.2, 1.0, 0.02], mat="glass", role="window")
}
`;export{e as default};
