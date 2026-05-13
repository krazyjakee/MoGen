// A small single-room house: four walls, a gabled roof, a door, a window.
//
// Walls are CSG `difference` of a box minus a door/window cutout so the
// openings are real geometry (not just a painted texture). The roof is
// built from two sloped boxes making an A-frame. A triangular prism on the
// front could replace the gable filler but we keep a simple plank.

material "wall"   (color=[0.92, 0.88, 0.78], metallic=0.0, roughness=0.85)
material "roof"   (color=[0.45, 0.18, 0.12], metallic=0.0, roughness=0.70)
material "wood"   (color=[0.55, 0.35, 0.18], metallic=0.0, roughness=0.75)
material "glass"  (color=[0.70, 0.85, 0.95], metallic=0.0, roughness=0.10)

scene {
  group "house" (role="building", tags="house") {

    // Floor slab.
    box "floor" (pos=[0, 0.05, 0], size=[4.0, 0.1, 3.0], mat="wood", role="floor")

    // Front wall with a door cutout (0.9 wide × 2.0 tall, centered on X).
    difference "front_wall" (pos=[0, 1.5, -1.45], mat="wall", role="wall") {
      box "wall"     (size=[4.0, 2.8, 0.1])
      box "door_gap" (pos=[0, -0.4, 0], size=[0.9, 2.0, 0.4])
    }

    // Back wall with a square window cutout.
    difference "back_wall" (pos=[0, 1.5, 1.45], mat="wall", role="wall") {
      box "wall"       (size=[4.0, 2.8, 0.1])
      box "window_gap" (pos=[0, 0.3, 0], size=[1.0, 1.0, 0.4])
    }

    // Side walls (no openings).
    box "left_wall"  (pos=[-1.95, 1.5, 0], size=[0.1, 2.8, 3.0], mat="wall", role="wall")
    box "right_wall" (pos=[ 1.95, 1.5, 0], size=[0.1, 2.8, 3.0], mat="wall", role="wall")

    // Gable roof: two sloped slabs forming an A-frame. Each is a long box
    // rotated 30° around Z from horizontal and pushed out to meet at the ridge.
    group "roof" (pos=[0, 2.93, 0], mat="roof", role="roof") {
      box "left_pitch"  (pos=[-0.86, 0.50, 0], rot=[0, 0,  30], size=[2.0, 0.08, 3.2])
      box "right_pitch" (pos=[ 0.86, 0.50, 0], rot=[0, 0, -30], size=[2.0, 0.08, 3.2])
    }

    // Door: swings on a hinge joint at its left edge. Positioned to fill the
    // front-wall cutout, offset so the box's local origin is at the hinge.
    group "door" (pos=[-0.45, 1.1, -1.45], role="door") {
      box "panel" (pos=[0.45, 0, 0], size=[0.9, 2.0, 0.04], mat="wood")
    }

    // Window pane filling the back-wall hole, inset slightly.
    box "window_pane" (pos=[0, 1.8, 1.45], size=[1.0, 1.0, 0.02], mat="glass", role="window")
  }
}

// Hinge the front door and open it 90° over 1.2 s.
joint "door_hinge" (type=hinge, axis=[0, 1, 0], limits=[0, 100], pivot="door")
clip "open_door" (seconds=1.2) {
  track "door_hinge" (from=0, to=90)
}
