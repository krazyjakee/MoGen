// mgen-generate seed=1776903121112551000
// prompt: the hand is not rigged correctly, please fix it.

material "skin_mat" (color=[0.82, 0.64, 0.55], metallic=0.0, roughness=0.7)

scene {
  skeleton "arm_skel" {
    bone "shoulder" (pos=[0, 0, 0], envelope=0.15) {
      bone "elbow" (pos=[0, 0.5, 0], envelope=0.12) {
        bone "wrist" (pos=[0, 0.5, 0], envelope=0.1) {
          bone "thumb_1" (pos=[0.06, 0.05, 0.02], envelope=0.02) {
            bone "thumb_2" (pos=[0.04, 0.04, 0], envelope=0.02)
          }
          bone "index_1" (pos=[0.06, 0.15, 0], envelope=0.02) {
            bone "index_2" (pos=[0, 0.05, 0], envelope=0.02) {
              bone "index_3" (pos=[0, 0.04, 0], envelope=0.02)
            }
          }
          bone "middle_1" (pos=[0.02, 0.16, 0], envelope=0.02) {
            bone "middle_2" (pos=[0, 0.06, 0], envelope=0.02) {
              bone "middle_3" (pos=[0, 0.05, 0], envelope=0.02)
            }
          }
          bone "ring_1" (pos=[-0.02, 0.15, 0], envelope=0.02) {
            bone "ring_2" (pos=[0, 0.05, 0], envelope=0.02) {
              bone "ring_3" (pos=[0, 0.04, 0], envelope=0.02)
            }
          }
          bone "pinky_1" (pos=[-0.06, 0.13, 0], envelope=0.02) {
            bone "pinky_2" (pos=[0, 0.04, 0], envelope=0.02) {
              bone "pinky_3" (pos=[0, 0.03, 0], envelope=0.02)
            }
          }
        }
      }
    }
  }

  cylinder "upper_arm" (
    pos=[0, 0.25, 0],
    radius=0.12,
    height=0.5,
    segments=16,
    mat="skin_mat",
    skin="arm_skel"
  )

  cylinder "lower_arm" (
    pos=[0, 0.75, 0],
    radius=0.1,
    height=0.5,
    segments=16,
    mat="skin_mat",
    skin="arm_skel"
  )

  sphere "wrist_mesh" (
    pos=[0, 1.0, 0],
    radius=0.09,
    mat="skin_mat",
    skin="arm_skel"
  )

  box "palm" (
    pos=[0, 1.08, 0],
    size=[0.16, 0.16, 0.04],
    mat="skin_mat",
    skin="arm_skel"
  )

  capsule "thumb_1_mesh" (pos=[0.08, 1.07, 0.02], radius=0.015, height=0.04, rot=[0, 0, -45], mat="skin_mat", skin="arm_skel")
  capsule "thumb_2_mesh" (pos=[0.11, 1.10, 0.02], radius=0.015, height=0.04, rot=[0, 0, -45], mat="skin_mat", skin="arm_skel")

  capsule "index_1_mesh" (pos=[0.06, 1.175, 0], radius=0.012, height=0.05, mat="skin_mat", skin="arm_skel")
  capsule "index_2_mesh" (pos=[0.06, 1.225, 0], radius=0.012, height=0.04, mat="skin_mat", skin="arm_skel")
  capsule "index_3_mesh" (pos=[0.06, 1.265, 0], radius=0.012, height=0.03, mat="skin_mat", skin="arm_skel")

  capsule "middle_1_mesh" (pos=[0.02, 1.19, 0], radius=0.013, height=0.06, mat="skin_mat", skin="arm_skel")
  capsule "middle_2_mesh" (pos=[0.02, 1.245, 0], radius=0.013, height=0.05, mat="skin_mat", skin="arm_skel")
  capsule "middle_3_mesh" (pos=[0.02, 1.29, 0], radius=0.013, height=0.04, mat="skin_mat", skin="arm_skel")

  capsule "ring_1_mesh" (pos=[-0.02, 1.175, 0], radius=0.012, height=0.05, mat="skin_mat", skin="arm_skel")
  capsule "ring_2_mesh" (pos=[-0.02, 1.225, 0], radius=0.012, height=0.04, mat="skin_mat", skin="arm_skel")
  capsule "ring_3_mesh" (pos=[-0.02, 1.265, 0], radius=0.012, height=0.03, mat="skin_mat", skin="arm_skel")

  capsule "pinky_1_mesh" (pos=[-0.06, 1.15, 0], radius=0.01, height=0.04, mat="skin_mat", skin="arm_skel")
  capsule "pinky_2_mesh" (pos=[-0.06, 1.19, 0], radius=0.01, height=0.03, mat="skin_mat", skin="arm_skel")
  capsule "pinky_3_mesh" (pos=[-0.06, 1.22, 0], radius=0.01, height=0.02, mat="skin_mat", skin="arm_skel")
}

clip "swing" (seconds=1.0) {
  track "elbow" (prop=rotation, from=0, to=60)
}

clip "bend_arm" (seconds=2.0) {
  track "elbow" (prop=rotation, axis=[1, 0, 0], keys=[[0, 0], [1.0, 90], [2.0, 0]])
  track "wrist" (prop=rotation, axis=[1, 0, 0], keys=[[0, 0], [1.0, 45], [2.0, 0]])
}

clip "fist" (seconds=2.0) {
  track "index_1" (prop=rotation, axis=[1, 0, 0], keys=[[0, 0], [1.0, 90], [2.0, 0]])
  track "index_2" (prop=rotation, axis=[1, 0, 0], keys=[[0, 0], [1.0, 90], [2.0, 0]])
  track "index_3" (prop=rotation, axis=[1, 0, 0], keys=[[0, 0], [1.0, 90], [2.0, 0]])
  track "middle_1" (prop=rotation, axis=[1, 0, 0], keys=[[0, 0], [1.0, 90], [2.0, 0]])
  track "middle_2" (prop=rotation, axis=[1, 0, 0], keys=[[0, 0], [1.0, 90], [2.0, 0]])
  track "middle_3" (prop=rotation, axis=[1, 0, 0], keys=[[0, 0], [1.0, 90], [2.0, 0]])
  track "ring_1" (prop=rotation, axis=[1, 0, 0], keys=[[0, 0], [1.0, 90], [2.0, 0]])
  track "ring_2" (prop=rotation, axis=[1, 0, 0], keys=[[0, 0], [1.0, 90], [2.0, 0]])
  track "ring_3" (prop=rotation, axis=[1, 0, 0], keys=[[0, 0], [1.0, 90], [2.0, 0]])
  track "pinky_1" (prop=rotation, axis=[1, 0, 0], keys=[[0, 0], [1.0, 90], [2.0, 0]])
  track "pinky_2" (prop=rotation, axis=[1, 0, 0], keys=[[0, 0], [1.0, 90], [2.0, 0]])
  track "pinky_3" (prop=rotation, axis=[1, 0, 0], keys=[[0, 0], [1.0, 90], [2.0, 0]])
  track "thumb_1" (prop=rotation, axis=[0, 1, 0], keys=[[0, 0], [1.0, -45], [2.0, 0]])
  track "thumb_2" (prop=rotation, axis=[0, 1, 0], keys=[[0, 0], [1.0, -45], [2.0, 0]])
}

clip "flex_fingers" (seconds=2.0) {
  track "index_1" (prop=rotation, axis=[1, 0, 0], keys=[[0, 0], [0.5, 45], [1.0, 0], [1.5, -10], [2.0, 0]])
  track "middle_1" (prop=rotation, axis=[1, 0, 0], keys=[[0, 0], [0.5, 45], [1.0, 0], [1.5, -10], [2.0, 0]])
  track "ring_1" (prop=rotation, axis=[1, 0, 0], keys=[[0, 0], [0.5, 45], [1.0, 0], [1.5, -10], [2.0, 0]])
  track "pinky_1" (prop=rotation, axis=[1, 0, 0], keys=[[0, 0], [0.5, 45], [1.0, 0], [1.5, -10], [2.0, 0]])
  track "thumb_1" (prop=rotation, axis=[0, 0, 1], keys=[[0, 0], [0.5, 20], [1.0, 0], [1.5, -10], [2.0, 0]])
}

clip "thumbs_up" (seconds=2.0) {
  track "index_1" (prop=rotation, axis=[1, 0, 0], keys=[[0, 0], [1.0, 90], [2.0, 0]])
  track "index_2" (prop=rotation, axis=[1, 0, 0], keys=[[0, 0], [1.0, 90], [2.0, 0]])
  track "index_3" (prop=rotation, axis=[1, 0, 0], keys=[[0, 0], [1.0, 90], [2.0, 0]])
  track "middle_1" (prop=rotation, axis=[1, 0, 0], keys=[[0, 0], [1.0, 90], [2.0, 0]])
  track "middle_2" (prop=rotation, axis=[1, 0, 0], keys=[[0, 0], [1.0, 90], [2.0, 0]])
  track "middle_3" (prop=rotation, axis=[1, 0, 0], keys=[[0, 0], [1.0, 90], [2.0, 0]])
  track "ring_1" (prop=rotation, axis=[1, 0, 0], keys=[[0, 0], [1.0, 90], [2.0, 0]])
  track "ring_2" (prop=rotation, axis=[1, 0, 0], keys=[[0, 0], [1.0, 90], [2.0, 0]])
  track "ring_3" (prop=rotation, axis=[1, 0, 0], keys=[[0, 0], [1.0, 90], [2.0, 0]])
  track "pinky_1" (prop=rotation, axis=[1, 0, 0], keys=[[0, 0], [1.0, 90], [2.0, 0]])
  track "pinky_2" (prop=rotation, axis=[1, 0, 0], keys=[[0, 0], [1.0, 90], [2.0, 0]])
  track "pinky_3" (prop=rotation, axis=[1, 0, 0], keys=[[0, 0], [1.0, 90], [2.0, 0]])
  track "thumb_1" (prop=rotation, axis=[0, 0, 1], keys=[[0, 0], [1.0, 45], [2.0, 0]])
  track "thumb_2" (prop=rotation, axis=[0, 0, 1], keys=[[0, 0], [1.0, 20], [2.0, 0]])
}