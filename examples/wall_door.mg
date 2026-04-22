// Wall with a doorway, carved by CSG difference (M6).
//
// The doorway is a box placed at the wall's +Y=-1 bottom and centered on X,
// sized larger in Z than the wall thickness so it punches through cleanly.

material "concrete" (color=[0.78, 0.78, 0.78], metallic=0.0, roughness=0.85)

scene {
  difference "wall_with_door" (mat="concrete", role="wall") {
    box "wall"    (size=[4.0, 3.0, 0.2])
    // Doorway: 0.9m wide, 2.0m tall, sitting on the bottom edge of the wall.
    // Extends beyond wall thickness so the cutout is unambiguous.
    box "doorway" (pos=[0, -0.5, 0], size=[0.9, 2.0, 0.5])
  }
}
