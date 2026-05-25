//! Fixed isometric preset shared between the wizard's viewer hint and the
//! headless thumbnail/screenshot path. Centralising the camera definition
//! keeps the screenshots fed to the LLM consistent with what the user sees
//! in the live viewer.

/// 30° pitch / 45° yaw is the classic 1:1:1 isometric pose; matches what
/// game engines call "true isometric". Distance is left to the framing pass
/// so the camera always fits the AABB.
pub const ISO_PITCH_RAD: f32 = std::f32::consts::PI * 30.0 / 180.0;
pub const ISO_YAW_RAD: f32 = std::f32::consts::FRAC_PI_4;

/// Spell out the preset as a `(yaw, pitch)` tuple — convenient when handing
/// frames to the capture pipeline.
pub fn iso_camera() -> (f32, f32) {
    (ISO_YAW_RAD, ISO_PITCH_RAD)
}
