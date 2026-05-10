const e=`meta (name = "coffee_cup", description = "A ceramic coffee mug filled with dark coffee", tags = ["prop", "drink", "cup"], seed = "1778114932383028400", thinking = "xhigh", prompt = "coffie cup", mogen_version = "0.1.2")

lod_scale (value=0.25)

material "ceramic" (color=[0.92, 0.92, 0.90], roughness=0.15)
material "coffee"  (color=[0.12, 0.06, 0.02], roughness=0.1)

scene {
  group "coffee_cup" {
    solid "mug_body" (mat="ceramic") {
      // Main cup wall
      tube "wall" (outer=0.04, inner=0.035, height=0.09)
      
      // Bottom plug. Centers at y=-0.04, spanning -0.045 to -0.035,
      // sealing the tube cleanly.
      cylinder "base" (y=-0.04, radius=0.04, height=0.01)
      
      // Curved handle on the +X side
      spline_tube "handle" (
        points=[
          [0.038,  0.025, 0],
          [0.065,  0.030, 0],
          [0.075,  0.000, 0],
          [0.065, -0.030, 0],
          [0.038, -0.025, 0]
        ],
        radius=0.006
      )
    }
    
    // Coffee liquid inside the mug.
    // Base top is at y=-0.035. The liquid spans from -0.035 to +0.035,
    // leaving a 1cm gap below the cup rim (at y=0.045).
    cylinder "liquid" (y=0.0, radius=0.0345, height=0.07, mat="coffee")
  }
}`;export{e as default};
