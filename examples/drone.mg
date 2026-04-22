// A four-rotor quadcopter drone.
//
// The central body is a flattened rounded_box; four arms extend to the corners
// with motor cylinders at their tips; each motor is a `rotor` group carrying a
// two-bladed propeller. A single `spin` template drives every propeller by
// targeting the shared array wrapper.
//
// The propellers ride as children of each rotor, so a rotor spin rotates both
// blades together.

material "carbon" (color=[0.08, 0.08, 0.10], metallic=0.2, roughness=0.55)
material "motor"  (color=[0.55, 0.55, 0.60], metallic=0.9, roughness=0.25)
material "blade"  (color=[0.85, 0.85, 0.90], metallic=0.0, roughness=0.35)

module "arm_with_rotor" (length=0.35, arm_thickness=0.02, motor_radius=0.04) {
  // Arm: long thin box along +X, starting at origin.
  box "arm" (pos=[$length * 0.5, 0, 0], size=[$length, $arm_thickness, $arm_thickness], mat="carbon", role="arm")

  // Motor + rotor sit at the arm's tip.
  cylinder "motor" (pos=[$length, 0.02, 0], radius=$motor_radius, height=0.03, mat="motor", role="motor")

  // Rotor floats a few mm above the motor — blades spin freely, so the gap
  // is intentional. Tag `floating` exempts the subtree from the connectivity
  // validator.
  group "rotor" (pos=[$length, 0.045, 0], role="rotor", tags="floating") {
    // Two blades, 180° apart, each a thin long box.
    array "blades" (count=2, around=y) {
      box "blade" (pos=[0.12, 0, 0], size=[0.24, 0.006, 0.02], mat="blade", role="blade")
    }
  }
}

scene {
  group "drone" (role="vehicle", tags="drone,quadcopter") {
    // Central body: flat rounded box.
    rounded_box "body" (size=[0.18, 0.05, 0.18], radius=0.02, mat="carbon", role="body")

    // Four arms arrayed around Y, rotated 90° per step.
    array "arms" (count=4, around=y, start_angle=45) {
      use "arm_with_rotor" (length=0.35, arm_thickness=0.02, motor_radius=0.04)
    }
  }
}
