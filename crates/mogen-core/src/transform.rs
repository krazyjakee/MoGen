use glam::{Mat4, Quat, Vec3};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct Transform {
    pub translation: Vec3,
    pub rotation: Quat,
    pub scale: Vec3,
}

impl Transform {
    pub const IDENTITY: Self = Self {
        translation: Vec3::ZERO,
        rotation: Quat::IDENTITY,
        scale: Vec3::ONE,
    };

    pub fn from_translation(t: Vec3) -> Self {
        Self { translation: t, ..Self::IDENTITY }
    }

    pub fn from_trs(translation: Vec3, rotation: Quat, scale: Vec3) -> Self {
        Self { translation, rotation, scale }
    }

    pub fn is_identity(&self) -> bool {
        self.translation == Vec3::ZERO
            && self.rotation == Quat::IDENTITY
            && self.scale == Vec3::ONE
    }

    pub fn to_mat4(&self) -> Mat4 {
        Mat4::from_scale_rotation_translation(self.scale, self.rotation, self.translation)
    }
}

impl Default for Transform {
    fn default() -> Self {
        Self::IDENTITY
    }
}
