// Chair built from parameterized modules (M5).
//
// Declares two modules — `leg` and `slab` — then composes them in a scene.
// Every `$param` in a module body is resolved against the caller's `use` args
// (falling back to the module's defaults) at expansion time.

material "wood"   (color=[0.55, 0.35, 0.18], metallic=0.0, roughness=0.75)
material "fabric" (color=[0.20, 0.30, 0.55], metallic=0.0, roughness=0.95)

// A single round leg with a connector on top where the seat rests.
module "leg" (height=0.5, radius=0.05) {
  cylinder "leg" (pos=[0, $height * 0.5, 0], radius=$radius, height=$height, mat="wood", role="leg") {
    connector "top" (at=[0, $height * 0.5, 0], dir=[0, 1, 0], tag=leg_top)
  }
}

// A flat rectangular slab; reused for both the seat and the back panel.
module "slab" (width=1.0, depth=1.0, thickness=0.1) {
  box "slab" (size=[$width, $thickness, $depth])
}

scene {
  group "chair" (role="furniture") {
    group "seat" (pos=[0, 0.5, 0], mat="fabric", role="seat") {
      use "slab" (width=1.0, depth=1.0, thickness=0.1)
    }

    group "back" (pos=[0, 1.0, -0.45], mat="wood", role="back") {
      use "slab" (width=1.0, depth=0.1, thickness=1.0)
    }

    // Four legs arrayed around Y; each is offset to a corner and then rotated
    // 90° per step by `array`, producing one at every corner.
    array "legs" (count=4, around=y) {
      group "offset" (pos=[0.45, 0, 0.45]) {
        use "leg" (height=0.5, radius=0.05)
      }
    }
  }
}
