//! Free-fly ("free cam") mode: a first-person editor camera the user drives
//! with WASD / arrow keys and right-drag mouse-look, the convention every
//! game engine uses for flying around a scene.
//!
//! Unlike the orbit camera (which pivots the eye around a fixed target) free
//! cam pivots the *look direction* around a fixed eye position. To keep the
//! rest of the viewport — picking, gizmos, the renderer — reading a single
//! `OrbitCamera`, free cam stays the authority on `pos`/`yaw`/`pitch` and each
//! frame writes an equivalent orbit pose back via [`FreeCam::apply_to`]: it
//! places the orbit target one focal distance ahead along the look vector and
//! solves the orbit angles so `OrbitCamera::eye()` lands exactly on `pos`.
//! Leaving free cam therefore needs no snapshot/restore — the orbit camera is
//! already framed around whatever the user was looking at.

use glam::Vec3;

use super::camera::OrbitCamera;

/// Mouse-look sensitivity in radians per pixel of right-drag.
const LOOK_SENS: f32 = 0.006;
/// Base movement speed as a multiple of the scene's fit distance per second,
/// so flying feels comparable across models of wildly different scale. This is
/// the multiplier at `speed_mult == 1.0`; the scroll wheel scales it live.
const MOVE_SPEED_FACTOR: f32 = 0.1;
/// Multiplier applied while Shift is held for quick traversal.
const BOOST: f32 = 4.0;
/// Bounds on the scroll-driven live speed multiplier.
const SPEED_MULT_MIN: f32 = 0.1;
const SPEED_MULT_MAX: f32 = 10.0;
/// Keep the look vector just shy of straight up/down so `forward × up` never
/// degenerates and the horizon can't flip.
const PITCH_LIMIT: f32 = 1.54;

pub struct FreeCam {
    pub active: bool,
    pos: Vec3,
    /// Azimuth. `forward = (cos(pitch)·sin(yaw), sin(pitch), cos(pitch)·cos(yaw))`.
    yaw: f32,
    pitch: f32,
    /// Live speed multiplier the scroll wheel drives, so the user can tune
    /// fly speed for the scene without a rebuild. Persists for the session.
    speed_mult: f32,
    /// True while a right-drag mouse-look gesture holds the cursor locked.
    /// The viewport uses this to edge-trigger the grab/hide and release/show
    /// viewport commands exactly once per gesture instead of every frame.
    looking: bool,
}

impl Default for FreeCam {
    fn default() -> Self {
        Self {
            active: false,
            pos: Vec3::ZERO,
            yaw: 0.0,
            pitch: 0.0,
            speed_mult: 1.0,
            looking: false,
        }
    }
}

impl FreeCam {
    /// Unit look direction in this mode's own spherical convention.
    fn forward(&self) -> Vec3 {
        Vec3::new(
            self.pitch.cos() * self.yaw.sin(),
            self.pitch.sin(),
            self.pitch.cos() * self.yaw.cos(),
        )
    }

    /// Latch the current orbit pose as the free-cam starting point so entering
    /// the mode never jumps the view: eye → `pos`, (target − eye) → look dir.
    pub fn enter(&mut self, cam: &OrbitCamera) {
        let eye = cam.eye();
        let fwd = (cam.target - eye).normalize_or_zero();
        self.pos = eye;
        self.pitch = fwd.y.clamp(-1.0, 1.0).asin();
        self.yaw = fwd.x.atan2(fwd.z);
        self.active = true;
        self.looking = false;
    }

    /// Whether a mouse-look gesture currently holds the cursor locked.
    pub fn is_looking(&self) -> bool {
        self.looking
    }

    /// Record the start/end of a mouse-look gesture. The viewport drives the
    /// matching `CursorGrab` / `CursorVisible` viewport commands; this only
    /// remembers the state so those commands fire once per gesture edge.
    pub fn set_looking(&mut self, looking: bool) {
        self.looking = looking;
    }

    /// Scroll wheel adjusts fly speed (Unreal/Unity scene-view convention):
    /// scroll up = faster, down = slower, clamped to a sane band.
    pub fn adjust_speed(&mut self, scroll_y: f32) {
        if scroll_y == 0.0 {
            return;
        }
        self.speed_mult =
            (self.speed_mult * (1.0 + scroll_y * 0.0015)).clamp(SPEED_MULT_MIN, SPEED_MULT_MAX);
    }

    /// Apply a right-drag delta (in pixels) as mouse-look. Non-inverted:
    /// drag right looks right, drag up looks up. Signs mirror the orbit
    /// handler so the two cameras feel consistent.
    pub fn look(&mut self, delta_px: glam::Vec2) {
        self.yaw -= delta_px.x * LOOK_SENS;
        self.pitch = (self.pitch - delta_px.y * LOOK_SENS).clamp(-PITCH_LIMIT, PITCH_LIMIT);
    }

    /// Advance the eye for one frame. `move_input` packs the pressed-key state
    /// as forward(+)/back(−) in `.x` and right(+)/left(−) in `.y`; movement
    /// follows the full 3D look vector so flying "toward where you're looking"
    /// climbs and dives with pitch. `fit_distance` scales speed to the scene.
    pub fn fly(&mut self, move_input: glam::Vec2, boost: bool, fit_distance: f32, dt: f32) {
        if move_input == glam::Vec2::ZERO {
            return;
        }
        let fwd = self.forward();
        let right = fwd.cross(Vec3::Y).normalize_or_zero();
        let dir = (fwd * move_input.x + right * move_input.y).normalize_or_zero();
        let speed = fit_distance.max(0.001)
            * MOVE_SPEED_FACTOR
            * self.speed_mult
            * if boost { BOOST } else { 1.0 };
        self.pos += dir * speed * dt.max(0.0);
    }

    /// Write an equivalent orbit pose so every consumer of `OrbitCamera`
    /// (renderer, picking, gizmos) sees the free-cam view. Keeps the orbit
    /// camera's `distance()` for sensible near/far planes; places the target
    /// that far ahead and solves the angles so `eye()` == `self.pos`.
    pub fn apply_to(&self, cam: &mut OrbitCamera) {
        let fwd = self.forward();
        let dist = cam.distance().max(0.001);
        cam.target = self.pos + fwd * dist;
        // eye = target + dist·spherical(yaw,pitch) must equal pos, i.e.
        // spherical = -fwd. Solve the orbit angles from that.
        cam.pitch = (-fwd.y).clamp(-1.0, 1.0).asin();
        cam.yaw = (-fwd.x).atan2(-fwd.z);
        // Hold a small near plane so flying close to interior walls doesn't
        // clip them, while keeping the far plane wide enough to span the whole
        // scene from any point inside it. Both scale with the scene's fit
        // distance so this works for tiny props and huge maps alike.
        let fit = cam.fit_distance.max(0.001);
        let near = (fit * 0.0006).clamp(0.02, 0.1);
        let far = fit * 3.0;
        cam.clip_override = Some((near, far));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn enter_then_apply_preserves_view() {
        let mut cam = OrbitCamera::default();
        cam.yaw = 1.1;
        cam.pitch = 0.3;
        let eye_before = cam.eye();
        let fwd_before = (cam.target - eye_before).normalize();

        let mut free = FreeCam::default();
        free.enter(&cam);
        free.apply_to(&mut cam);

        let eye_after = cam.eye();
        let fwd_after = (cam.target - eye_after).normalize();
        assert!((eye_after - eye_before).length() < 1e-4, "eye moved");
        assert!((fwd_after - fwd_before).length() < 1e-4, "look dir changed");
    }

    #[test]
    fn forward_movement_follows_look_vector() {
        let mut free = FreeCam::default();
        free.yaw = 0.0;
        free.pitch = 0.0;
        let start = free.pos;
        free.fly(glam::vec2(1.0, 0.0), false, 1.0, 1.0);
        let moved = free.pos - start;
        // Looking down +Z at yaw=pitch=0, W should advance along +Z only.
        assert!(moved.z > 0.0);
        assert!(moved.x.abs() < 1e-5 && moved.y.abs() < 1e-5);
    }

    #[test]
    fn pitch_is_clamped_under_repeated_look() {
        let mut free = FreeCam::default();
        for _ in 0..1000 {
            free.look(glam::vec2(0.0, -100.0));
        }
        assert!(free.pitch <= PITCH_LIMIT + 1e-6);
    }

    #[test]
    fn strafe_right_is_perpendicular_to_forward() {
        // At yaw=π (maps to orbit yaw=0, looking toward -Z from +Z eye),
        // strafing right (mv.y=+1) must produce movement perpendicular to
        // forward and parallel to the world X axis.
        let mut free = FreeCam::default();
        free.yaw = std::f32::consts::PI;
        free.pitch = 0.0;
        let start = free.pos;
        free.fly(glam::vec2(0.0, 1.0), false, 1.0, 1.0);
        let moved = free.pos - start;
        assert!(moved.z.abs() < 1e-5, "strafe must not move forward/back");
        assert!(moved.y.abs() < 1e-5, "strafe must not move vertically");
        assert!(moved.x.abs() > 1e-5, "strafe must move horizontally");
    }

    #[test]
    fn adjust_speed_clamps_at_min_and_max() {
        let mut free = FreeCam::default();
        // Scroll down extremely far — must not go below SPEED_MULT_MIN.
        for _ in 0..100 {
            free.adjust_speed(-10_000.0);
        }
        assert!(
            free.speed_mult >= SPEED_MULT_MIN - 1e-6,
            "speed_mult {} went below SPEED_MULT_MIN {}",
            free.speed_mult,
            SPEED_MULT_MIN
        );
        // Scroll up extremely far — must not go above SPEED_MULT_MAX.
        for _ in 0..100 {
            free.adjust_speed(10_000.0);
        }
        assert!(
            free.speed_mult <= SPEED_MULT_MAX + 1e-6,
            "speed_mult {} exceeded SPEED_MULT_MAX {}",
            free.speed_mult,
            SPEED_MULT_MAX
        );
    }

    #[test]
    fn enter_with_offset_target_preserves_view() {
        // Orbit camera aimed at a non-origin target — `enter` must derive the
        // correct look direction from (target − eye), not from the raw angles.
        let mut cam = OrbitCamera::default();
        cam.target = glam::Vec3::new(3.0, 1.0, -2.0);
        cam.yaw = 0.8;
        cam.pitch = 0.2;
        let eye_before = cam.eye();
        let fwd_before = (cam.target - eye_before).normalize();

        let mut free = FreeCam::default();
        free.enter(&cam);
        free.apply_to(&mut cam);

        let eye_after = cam.eye();
        let fwd_after = (cam.target - eye_after).normalize();
        assert!(
            (eye_after - eye_before).length() < 1e-3,
            "eye jumped after enter+apply with offset target"
        );
        assert!(
            (fwd_after - fwd_before).length() < 1e-3,
            "look direction changed after enter+apply with offset target"
        );
    }
}
