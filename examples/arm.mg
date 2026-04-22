// Skinned 3-bone arm. A cylindrical arm mesh deforms by the bone hierarchy
// below it; the `swing` clip bends the elbow around +X so the forearm sweeps.
//
// Coordinate setup: the arm lies along +Y with its base at y=0 and tip at
// y=1.5. Bones sit at 0.0, 0.5, and 1.0 so the vertex closest to each bone
// falls inside its envelope.

material "skin_mat" (color=[0.82, 0.64, 0.55], metallic=0.0, roughness=0.7)

scene {
  skeleton "arm_skel" {
    bone "shoulder" (pos=[0, 0, 0], envelope=0.75) {
      bone "elbow" (pos=[0, 0.5, 0], envelope=0.75) {
        bone "wrist" (pos=[0, 0.5, 0], envelope=0.75)
      }
    }
  }

  cylinder "arm_mesh" (
    pos=[0, 0.75, 0],
    radius=0.12,
    height=1.5,
    segments=16,
    mat="skin_mat",
    skin="arm_skel",
  )
}

// Bend the elbow ~60° around +X over one second. `track` targets the bone
// scene node directly; `prop=rotation` drives the node's TRS rotation channel.
clip "swing" (seconds=1.0) {
  track "elbow" (prop=rotation, from=0, to=60)
}
